namespace Insight.Identity.Domain.Services;

/// <summary>
/// Phase 1 lookup: resolve email → <c>person_id</c>, hydrate observations,
/// assemble the response. Subordinate hydration is wired in but capped at
/// the direct-children level until Phase 2 enables recursive expansion via
/// <see cref="LookupOptions.ExpandSubordinates"/>.
/// </summary>
public sealed class PersonLookupService
{
    private readonly IPersonsReader _reader;

    public PersonLookupService(IPersonsReader reader)
    {
        _reader = reader;
    }

    public async Task<Person?> GetByEmailAsync(
        Guid tenantId,
        string email,
        LookupOptions options,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(email);
        // No case normalisation: per ADR-0011 the `persons.value_id`
        // column collation is `utf8mb4_unicode_ci`, so the SQL
        // comparison tolerates case differences regardless of how the
        // caller typed the email. Trim is still useful for stray
        // whitespace from URL paths.
        var emailKey = email.Trim();

        var personId = await _reader.ResolvePersonIdByEmailAsync(tenantId, emailKey, cancellationToken)
            .ConfigureAwait(false);
        if (personId is null)
        {
            return null;
        }

        return await BuildAsync(tenantId, personId.Value, options, depth: 0, visited: new HashSet<Guid>(), cancellationToken)
            .ConfigureAwait(false);
    }

    private async Task<Person?> BuildAsync(
        Guid tenantId,
        Guid personId,
        LookupOptions options,
        int depth,
        HashSet<Guid> visited,
        CancellationToken cancellationToken)
    {
        if (!visited.Add(personId))
        {
            return null;
        }

        var observations = await _reader
            .GetLatestObservationsAsync(tenantId, personId, cancellationToken)
            .ConfigureAwait(false);
        if (observations.Count == 0)
        {
            return null;
        }

        IReadOnlyList<Person> subordinates = Array.Empty<Person>();
        if (options.ExpandSubordinates && depth < options.MaxDepth)
        {
            var childIds = await _reader
                .GetDirectSubordinateIdsAsync(tenantId, personId, cancellationToken)
                .ConfigureAwait(false);
            if (childIds.Count > 0)
            {
                var children = new List<Person>(childIds.Count);
                foreach (var childId in childIds)
                {
                    var built = await BuildAsync(tenantId, childId, options, depth + 1, visited, cancellationToken)
                        .ConfigureAwait(false);
                    if (built is not null)
                    {
                        children.Add(built);
                    }
                }
                subordinates = children;
            }
        }

        return PersonAssembler.Assemble(personId, observations, subordinates);
    }
}

/// <summary>
/// Lookup behaviour switches. Phase 1 calls keep
/// <see cref="ExpandSubordinates"/> false; Phase 2 enables it with a
/// guarded <see cref="MaxDepth"/>.
/// </summary>
public sealed record LookupOptions(bool ExpandSubordinates, int MaxDepth)
{
    public static readonly LookupOptions Phase1 = new(ExpandSubordinates: false, MaxDepth: 0);
}
