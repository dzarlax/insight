namespace Insight.Identity.Domain.Services;

/// <summary>
/// Email lookup returning a <see cref="Person"/> with the org-tree
/// drawn from <c>org_chart</c> filtered to <see cref="LookupOptions.OrgChartSourceType"/>.
/// </summary>
public sealed class PersonLookupService
{
    private readonly IPersonsReader _reader;
    private readonly IVisibilityReader _visibility;

    public PersonLookupService(IPersonsReader reader, IVisibilityReader visibility)
    {
        _reader = reader;
        _visibility = visibility;
    }

    /// <summary>Lookup by email. Returns <c>null</c> when no current observation matches.</summary>
    public async Task<Person?> GetByEmailAsync(
        Guid tenantId,
        string email,
        LookupOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(email);
        ArgumentNullException.ThrowIfNull(options);
        // ADR-0011: value_id collation is utf8mb4_unicode_ci, so the SQL
        // comparison handles case. Trim only strips stray whitespace.
        var emailKey = email.Trim();

        var personId = await _reader.ResolvePersonIdByEmailAsync(tenantId, emailKey, cancellationToken)
            .ConfigureAwait(false);
        if (personId is null)
        {
            return null;
        }

        var visited = new HashSet<Guid>();
        // Observations are discarded — only the profile path needs them
        // again (for the ids[] / Profile assembler). GetByEmail returns
        // just the Person.
        var (root, _) = await HydrateAsync(tenantId, personId.Value, options, depth: 0, visited, cancellationToken)
            .ConfigureAwait(false);
        return root;
    }

    /// <summary>
    /// Hydrate the org-tree for a caller that already resolved the
    /// <c>person_id</c>. Returns the assembled <see cref="Person"/>
    /// together with the observations used to build it so the caller
    /// can reuse them without a second DB round-trip.
    /// </summary>
    public async Task<(Person? Person, IReadOnlyList<PersonObservation> Observations)> HydrateForProfileAsync(
        Guid tenantId,
        Guid personId,
        LookupOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(options);
        var visited = new HashSet<Guid>();
        return await HydrateAsync(tenantId, personId, options, depth: 0, visited, cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>
    /// Build the caller's accessible org forest: the caller's own subtree
    /// plus the subtree rooted at every active visibility grant. A
    /// whole-tenant grant (<c>viewed_person_id IS NULL</c>) expands to
    /// every tenant root. Granted roots are appended to the caller's
    /// <see cref="Person.Subordinates"/>; a single <c>visited</c> set is
    /// shared across the whole walk so an overlapping or revisited
    /// person never appears twice. Source filtering matches
    /// <see cref="LookupOptions.OrgChartSourceType"/>. Returns
    /// <c>null</c> when the caller has no observations.
    /// </summary>
    public async Task<Person?> GetVisibleForestAsync(
        Guid tenantId,
        Guid viewerPersonId,
        LookupOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(options);

        // Hydrate the caller first so their own node anchors the tree and
        // their subtree is marked visited before any grant is expanded.
        var visited = new HashSet<Guid>();
        var (top, _) = await HydrateAsync(tenantId, viewerPersonId, options, depth: 0, visited, cancellationToken)
            .ConfigureAwait(false);
        if (top is null)
        {
            return null;
        }

        var grants = await _visibility
            .GetActiveVisibilityGrantsByViewerAsync(tenantId, viewerPersonId, cancellationToken)
            .ConfigureAwait(false);

        IReadOnlyList<Guid> grantedRoots;
        if (grants.Any(static g => g.ViewedPersonId is null))
        {
            // Whole-tenant grant: expand from every root of the tenant forest.
            grantedRoots = await _reader
                .GetRootPersonIdsAsync(tenantId, options.OrgChartSourceType, cancellationToken)
                .ConfigureAwait(false);
        }
        else
        {
            var named = grants
                .Where(static g => g.ViewedPersonId is not null)
                .Select(static g => g.ViewedPersonId!.Value)
                .Distinct()
                .ToList();
            grantedRoots = await PruneSubsumedRootsAsync(
                    tenantId, viewerPersonId, named, options.OrgChartSourceType, cancellationToken)
                .ConfigureAwait(false);
        }

        if (grantedRoots.Count == 0)
        {
            return top;
        }

        var extra = new List<Person>();
        foreach (var rootId in grantedRoots)
        {
            // Shared `visited` skips the caller's own node and any subtree
            // already emitted, so overlapping grants never duplicate a person.
            var (built, _) = await HydrateAsync(tenantId, rootId, options, depth: 0, visited, cancellationToken)
                .ConfigureAwait(false);
            if (built is not null)
            {
                extra.Add(built);
            }
        }

        if (extra.Count == 0)
        {
            return top;
        }

        var combined = new List<Person>(top.Subordinates.Count + extra.Count);
        combined.AddRange(top.Subordinates);
        combined.AddRange(extra);
        return top with { Subordinates = combined };
    }

    /// <summary>
    /// Drop any candidate root whose subtree is already covered by another
    /// anchor — the viewer or another candidate that is its ancestor in
    /// the source tree. Keeps only the maximal roots so the forest has no
    /// nested duplicates.
    /// </summary>
    private async Task<IReadOnlyList<Guid>> PruneSubsumedRootsAsync(
        Guid tenantId,
        Guid viewerPersonId,
        List<Guid> candidates,
        string sourceType,
        CancellationToken cancellationToken)
    {
        if (candidates.Count == 0)
        {
            return candidates;
        }

        var anchors = new HashSet<Guid>(candidates) { viewerPersonId };
        var kept = new List<Guid>(candidates.Count);
        foreach (var candidate in candidates)
        {
            if (!await HasAncestorInAsync(tenantId, candidate, anchors, sourceType, cancellationToken)
                    .ConfigureAwait(false))
            {
                kept.Add(candidate);
            }
        }
        return kept;
    }

    /// <summary>
    /// Walk parents up from <paramref name="personId"/> within
    /// <paramref name="sourceType"/>; returns <c>true</c> when an ancestor
    /// is present in <paramref name="anchors"/>. A local guard set bounds
    /// pathological cycles.
    /// </summary>
    private async Task<bool> HasAncestorInAsync(
        Guid tenantId,
        Guid personId,
        HashSet<Guid> anchors,
        string sourceType,
        CancellationToken cancellationToken)
    {
        var current = personId;
        var guard = new HashSet<Guid> { current };
        while (true)
        {
            var parentEdges = await _reader
                .GetCurrentParentsAsync(tenantId, current, cancellationToken)
                .ConfigureAwait(false);
            var parentEdge = FilterToSource(parentEdges, sourceType);
            if (parentEdge is null)
            {
                return false;
            }
            var parentId = parentEdge.ParentPersonId;
            if (anchors.Contains(parentId))
            {
                return true;
            }
            if (!guard.Add(parentId))
            {
                return false;
            }
            current = parentId;
        }
    }

    private async Task<(Person? Person, IReadOnlyList<PersonObservation> Observations)> HydrateAsync(
        Guid tenantId,
        Guid personId,
        LookupOptions options,
        int depth,
        HashSet<Guid> visited,
        CancellationToken cancellationToken)
    {
        if (!visited.Add(personId))
        {
            return (null, Array.Empty<PersonObservation>());
        }

        var observations = await _reader
            .GetLatestObservationsAsync(tenantId, personId, cancellationToken)
            .ConfigureAwait(false);
        if (observations.Count == 0)
        {
            return (null, Array.Empty<PersonObservation>());
        }

        // Parent is always hydrated when an org_chart edge exists — it
        // is a single O(1) lookup and the legacy parent_* contract is
        // unconditional. Only subordinates recursion is gated by
        // ExpandSubordinates.
        ParentProjection? parent = null;
        var parentEdges = await _reader
            .GetCurrentParentsAsync(tenantId, personId, cancellationToken)
            .ConfigureAwait(false);
        var parentEdge = FilterToSource(parentEdges, options.OrgChartSourceType);
        if (parentEdge is not null)
        {
            parent = await ResolveParentAsync(tenantId, parentEdge, options.OrgChartSourceType, cancellationToken)
                .ConfigureAwait(false);
        }

        IReadOnlyList<Person> subordinates = Array.Empty<Person>();
        if (options.ExpandSubordinates && depth < options.MaxDepth)
        {
            var childEdges = await _reader
                .GetCurrentChildrenAsync(tenantId, personId, cancellationToken)
                .ConfigureAwait(false);
            var childIds = childEdges
                .Where(e => string.Equals(e.InsightSourceType, options.OrgChartSourceType, StringComparison.Ordinal))
                .Select(e => e.ChildPersonId)
                .Distinct()
                .ToList();
            if (childIds.Count > 0)
            {
                var children = new List<Person>(childIds.Count);
                foreach (var childId in childIds)
                {
                    var (built, _) = await HydrateAsync(tenantId, childId, options, depth + 1, visited, cancellationToken)
                        .ConfigureAwait(false);
                    if (built is not null)
                    {
                        children.Add(built);
                    }
                }
                subordinates = children;
            }
        }

        var assembled = PersonAssembler.Assemble(personId, observations, parent, subordinates);
        return (assembled, observations);
    }

    private async Task<ParentProjection> ResolveParentAsync(
        Guid tenantId,
        OrgChartEdge edge,
        string sourceType,
        CancellationToken cancellationToken)
    {
        var parentObservations = await _reader
            .GetLatestObservationsAsync(tenantId, edge.ParentPersonId, cancellationToken)
            .ConfigureAwait(false);

        var latest = parentObservations
            .GroupBy(static o => o.ValueType, StringComparer.Ordinal)
            .ToDictionary(
                static g => g.Key,
                static g => g.OrderByDescending(static o => o.CreatedAt).First().ValueEffective,
                StringComparer.Ordinal);

        var email = latest.GetValueOrDefault(ValueTypes.Email);
        var displayName = latest.GetValueOrDefault(ValueTypes.DisplayName);

        var parentIds = await _reader
            .GetCurrentSourceIdsAsync(tenantId, edge.ParentPersonId, cancellationToken)
            .ConfigureAwait(false);
        var sourceNativeId = parentIds
            .FirstOrDefault(s =>
                string.Equals(s.InsightSourceType, sourceType, StringComparison.Ordinal)
                && s.InsightSourceId == edge.InsightSourceId)
            ?.Value;

        return new ParentProjection(
            PersonId: edge.ParentPersonId,
            Email: email,
            DisplayName: displayName,
            SourceNativeId: sourceNativeId);
    }

    private static OrgChartEdge? FilterToSource(IReadOnlyList<OrgChartEdge> edges, string sourceType)
    {
        for (var i = 0; i < edges.Count; i++)
        {
            if (string.Equals(edges[i].InsightSourceType, sourceType, StringComparison.Ordinal))
            {
                return edges[i];
            }
        }
        return null;
    }
}

/// <summary>
/// Lookup behaviour switches passed from the Api layer into the domain
/// services. <see cref="ExpandSubordinates"/> is the only kill-switch
/// here — parent is always hydrated when an <c>org_chart</c> edge exists.
/// </summary>
public sealed record LookupOptions(
    bool ExpandSubordinates,
    int MaxDepth,
    string OrgChartSourceType)
{
    /// <summary>
    /// Test-only convenience: expand subordinates from BambooHR,
    /// depth-capped at 16. Production paths bind from
    /// <c>AppOptions</c> via <c>PersonsEndpoints.BuildLookupOptions</c>.
    /// </summary>
    public static readonly LookupOptions Default =
        new(ExpandSubordinates: true, MaxDepth: 16, OrgChartSourceType: "bamboohr");
}

/// <summary>Parent edge resolved into the fields the assembler writes onto the response.</summary>
public sealed record ParentProjection(
    Guid PersonId,
    string? Email,
    string? DisplayName,
    string? SourceNativeId);
