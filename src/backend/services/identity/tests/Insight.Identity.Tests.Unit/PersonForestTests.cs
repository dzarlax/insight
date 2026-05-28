using FluentAssertions;
using Insight.Identity.Domain;
using Insight.Identity.Domain.Services;
using Xunit;

namespace Insight.Identity.Tests.Unit;

/// <summary>
/// Unit coverage for <see cref="PersonLookupService.GetVisibleForestAsync"/>:
/// the caller's own subtree unioned with the subtree of every active
/// visibility grant. Exercises whole-tenant expansion, maximal-root
/// pruning (a granted root subsumed by the viewer or another grant is
/// dropped), and shared-visited de-duplication of overlapping subtrees.
/// </summary>
public sealed class PersonForestTests
{
    private static readonly Guid TenantId = Guid.Parse("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    private static readonly Guid SourceId = Guid.Parse("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");

    private static readonly Guid V = Guid.Parse("00000000-0000-0000-0000-0000000000a0"); // viewer
    private static readonly Guid R = Guid.Parse("00000000-0000-0000-0000-0000000000b0"); // tree root
    private static readonly Guid A = Guid.Parse("00000000-0000-0000-0000-0000000000c0");
    private static readonly Guid B = Guid.Parse("00000000-0000-0000-0000-0000000000d0");
    private static readonly Guid C = Guid.Parse("00000000-0000-0000-0000-0000000000e0");

    private static readonly LookupOptions Options = LookupOptions.Default;

    private static readonly string[] ViewerOnly = { "v" };
    private static readonly string[] WholeTree = { "v", "r", "a", "b", "c" };
    private static readonly string[] ViewerPlusASubtree = { "v", "a", "c" };
    private static readonly string[] RootSubtree = { "r", "a", "b", "c" };

    [Fact]
    public async Task No_grant_returns_only_the_caller()
    {
        var reader = BuildTree();
        var svc = new PersonLookupService(reader, Grants());

        var forest = await svc.GetVisibleForestAsync(TenantId, V, Options, CancellationToken.None);

        forest.Should().NotBeNull();
        Flatten(forest!).Should().BeEquivalentTo(ViewerOnly);
    }

    [Fact]
    public async Task Caller_without_observations_returns_null()
    {
        var reader = BuildTree();
        var svc = new PersonLookupService(reader, Grants());

        var forest = await svc.GetVisibleForestAsync(
            TenantId, Guid.Parse("00000000-0000-0000-0000-0000000000ff"), Options, CancellationToken.None);

        forest.Should().BeNull();
    }

    [Fact]
    public async Task Whole_tenant_grant_expands_every_tenant_root()
    {
        var reader = BuildTree();
        var svc = new PersonLookupService(reader, Grants(WholeTenant()));

        var forest = await svc.GetVisibleForestAsync(TenantId, V, Options, CancellationToken.None);

        Flatten(forest!).Should().BeEquivalentTo(WholeTree);
    }

    [Fact]
    public async Task Grant_subsumed_by_another_grant_is_not_duplicated()
    {
        // Grants on A and C, but C is A's descendant: prune keeps only A,
        // and C still appears exactly once inside A's subtree.
        var reader = BuildTree();
        var svc = new PersonLookupService(reader, Grants(On(A), On(C)));

        var forest = await svc.GetVisibleForestAsync(TenantId, V, Options, CancellationToken.None);

        Flatten(forest!).Should().BeEquivalentTo(ViewerPlusASubtree);
    }

    [Fact]
    public async Task Grant_subsumed_by_caller_subtree_is_not_duplicated()
    {
        // Viewer is R (tree root) and also holds a grant on A (R's child).
        // A is already in the viewer's own subtree, so the grant adds
        // nothing and A appears once.
        var reader = BuildTree();
        var svc = new PersonLookupService(reader, Grants(On(A)));

        var forest = await svc.GetVisibleForestAsync(TenantId, R, Options, CancellationToken.None);

        Flatten(forest!).Should().BeEquivalentTo(RootSubtree);
    }

    // ── Fixture ─────────────────────────────────────────────────────

    // Tree (bamboohr): R → (A, B); A → C. V is isolated (no edges).
    private static StubReader BuildTree()
    {
        var reader = new StubReader();
        reader.AddPerson(V, "v");
        reader.AddPerson(R, "r");
        reader.AddPerson(A, "a");
        reader.AddPerson(B, "b");
        reader.AddPerson(C, "c");
        reader.AddEdge(child: A, parent: R);
        reader.AddEdge(child: B, parent: R);
        reader.AddEdge(child: C, parent: A);
        reader.Roots.Add(R);
        return reader;
    }

    private static StubVisibility Grants(params Visibility[] grants) => new(grants);

    private static Visibility On(Guid viewed) => Make(viewed);

    private static Visibility WholeTenant() => Make(null);

    private static Visibility Make(Guid? viewed) => new(
        VisibilityId: Guid.NewGuid(),
        InsightTenantId: TenantId,
        ViewerPersonId: V,
        ViewedPersonId: viewed,
        ValidFrom: DateTime.UtcNow.AddDays(-1),
        ValidTo: null,
        AuthorPersonId: Guid.Empty,
        Reason: null,
        CreatedAt: DateTime.UtcNow.AddDays(-1));

    private static List<string> Flatten(Person root)
    {
        var acc = new List<string>();
        Walk(root, acc);
        return acc;

        static void Walk(Person p, List<string> into)
        {
            into.Add(p.Email);
            foreach (var child in p.Subordinates)
            {
                Walk(child, into);
            }
        }
    }

    private sealed class StubReader : IPersonsReader
    {
        private readonly Dictionary<Guid, List<PersonObservation>> _observations = new();
        private readonly Dictionary<Guid, List<OrgChartEdge>> _parents = new();
        private readonly Dictionary<Guid, List<OrgChartEdge>> _children = new();

        public List<Guid> Roots { get; } = new();

        public void AddPerson(Guid personId, string email) =>
            _observations[personId] = new List<PersonObservation>
            {
                new(personId, "bamboohr", SourceId, ValueTypes.Email, email, DateTime.UtcNow),
            };

        public void AddEdge(Guid child, Guid parent)
        {
            var edge = new OrgChartEdge("bamboohr", SourceId, child, parent, DateTime.UtcNow);
            (_parents.TryGetValue(child, out var p) ? p : _parents[child] = new()).Add(edge);
            (_children.TryGetValue(parent, out var c) ? c : _children[parent] = new()).Add(edge);
        }

        public Task<IReadOnlyList<PersonObservation>> GetLatestObservationsAsync(Guid tenantId, Guid personId, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<PersonObservation>>(
                _observations.TryGetValue(personId, out var o) ? o : Array.Empty<PersonObservation>());

        public Task<IReadOnlyList<OrgChartEdge>> GetCurrentParentsAsync(Guid tenantId, Guid childPersonId, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<OrgChartEdge>>(
                _parents.TryGetValue(childPersonId, out var e) ? e : Array.Empty<OrgChartEdge>());

        public Task<IReadOnlyList<OrgChartEdge>> GetCurrentChildrenAsync(Guid tenantId, Guid parentPersonId, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<OrgChartEdge>>(
                _children.TryGetValue(parentPersonId, out var e) ? e : Array.Empty<OrgChartEdge>());

        public Task<IReadOnlyList<Guid>> GetRootPersonIdsAsync(Guid tenantId, string orgChartSourceType, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<Guid>>(Roots);

        public Task<Guid?> ResolvePersonIdByEmailAsync(Guid tenantId, string email, CancellationToken cancellationToken)
            => Task.FromResult<Guid?>(null);

        public Task<IReadOnlyList<Guid>> ResolvePersonIdsByEmailAsync(Guid tenantId, string email, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<Guid>>(Array.Empty<Guid>());

        public Task<IReadOnlyList<Guid>> ResolvePersonIdsBySourceIdAsync(Guid tenantId, string sourceType, Guid sourceId, string value, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<Guid>>(Array.Empty<Guid>());

        public Task<IReadOnlyList<PersonSourceId>> GetCurrentSourceIdsAsync(Guid tenantId, Guid personId, CancellationToken cancellationToken)
            => Task.FromResult<IReadOnlyList<PersonSourceId>>(Array.Empty<PersonSourceId>());

        public Task<Guid?> ResolvePersonIdByAccountIdAsync(Guid tenantId, string accountId, CancellationToken cancellationToken)
            => Task.FromResult<Guid?>(null);
    }

    private sealed class StubVisibility : IVisibilityReader
    {
        private readonly IReadOnlyList<Visibility> _grants;

        public StubVisibility(IReadOnlyList<Visibility> grants) => _grants = grants;

        public Task<IReadOnlyList<Visibility>> GetActiveVisibilityGrantsByViewerAsync(Guid tenantId, Guid viewerPersonId, CancellationToken cancellationToken)
            => Task.FromResult(_grants);

        public Task<bool> IsTargetInVisibleSetAsync(Guid tenantId, Guid viewerPersonId, Guid targetPersonId, string orgChartSourceType, CancellationToken cancellationToken)
            => Task.FromResult(false);

        public Task<Visibility?> GetByIdAsync(Guid tenantId, Guid visibilityId, CancellationToken cancellationToken)
            => Task.FromResult<Visibility?>(null);

        public Task<PagedResult<Visibility>> ListAsync(Guid tenantId, Guid? filterByViewer, Guid? filterByViewed, bool activeOnly, PageRequest page, CancellationToken cancellationToken)
            => throw new NotSupportedException("ListAsync is not used by PersonForestTests.");
    }
}
