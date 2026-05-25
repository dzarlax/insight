using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using FluentAssertions;
using Insight.Identity.Api.Contracts;
using Insight.Identity.Domain.Services;
using MySqlConnector;
using Xunit;

namespace Insight.Identity.Tests.Integration;

/// <summary>
/// End-to-end tests for the OrgChart Visibility CRUD endpoints on
/// /v1/visibility, /v1/roles, /v1/person-roles. Covers admin-gating
/// (401/400/403), happy-path 201/200/204, validators, role-in-use guard.
/// </summary>
[Collection(MariaDbCollection.Name)]
public sealed class OrgChartVisibilityEndpointsTests : IAsyncLifetime
{
    private static readonly Guid TenantId       = Guid.Parse("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    private static readonly Guid AdminPersonId  = Guid.Parse("00000000-0000-0000-0000-000000000a01");
    private static readonly Guid NonAdminId     = Guid.Parse("00000000-0000-0000-0000-000000000b02");
    private static readonly Guid ViewerPersonId = Guid.Parse("11111111-1111-1111-1111-111111111111");
    private static readonly Guid ViewedPersonId = Guid.Parse("22222222-2222-2222-2222-222222222222");

    private readonly MariaDbFixture _fixture;
    private TestApplicationFactory? _adminApp;
    private TestApplicationFactory? _nonAdminApp;
    private TestApplicationFactory? _anonApp;

    public OrgChartVisibilityEndpointsTests(MariaDbFixture fixture) => _fixture = fixture;

    public async Task InitializeAsync()
    {
        await _fixture.ResetAsync().ConfigureAwait(false);
        // Grant the test admin caller the admin role in this tenant.
        await InsertPersonRoleAsync(AdminPersonId, Roles.Admin).ConfigureAwait(false);

        _adminApp    = new TestApplicationFactory(_fixture.ConnectionString, TenantId, defaultCallerPersonId: AdminPersonId);
        _nonAdminApp = new TestApplicationFactory(_fixture.ConnectionString, TenantId, defaultCallerPersonId: NonAdminId);
        _anonApp     = new TestApplicationFactory(_fixture.ConnectionString, TenantId, defaultCallerPersonId: null);
    }

    public Task DisposeAsync()
    {
        _adminApp?.Dispose();
        _nonAdminApp?.Dispose();
        _anonApp?.Dispose();
        return Task.CompletedTask;
    }

    // ── Gate behaviour (applies to every endpoint identically) ──────

    [Fact]
    public async Task Post_visibility_without_caller_returns_401()
    {
        var client = _anonApp!.CreateClient();
        var body = new CreateVisibilityCommandModel(ViewerPersonId, ViewedPersonId, null, "test");
        var response = await client.PostJsonAsync("/v1/visibility", body).ConfigureAwait(false);
        response.StatusCode.Should().Be(HttpStatusCode.Unauthorized);
    }

    [Fact]
    public async Task Post_visibility_as_non_admin_returns_403()
    {
        var client = _nonAdminApp!.CreateClient();
        var body = new CreateVisibilityCommandModel(ViewerPersonId, ViewedPersonId, null, "test");
        var response = await client.PostJsonAsync("/v1/visibility", body).ConfigureAwait(false);
        response.StatusCode.Should().Be(HttpStatusCode.Forbidden);
        var doc = await response.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        doc.GetProperty("type").GetString().Should().Be("urn:insight:error:admin_required");
    }

    // ── /v1/visibility CRUD ─────────────────────────────────────────

    [Fact]
    public async Task Visibility_create_list_delete_round_trip_as_admin()
    {
        var client = _adminApp!.CreateClient();

        // POST
        var body = new CreateVisibilityCommandModel(ViewerPersonId, ViewedPersonId, null, "scoped grant");
        var post = await client.PostJsonAsync("/v1/visibility", body).ConfigureAwait(false);
        post.StatusCode.Should().Be(HttpStatusCode.Created);
        var created = await post.ReadJsonAsync<VisibilityResponse>().ConfigureAwait(false);
        created!.ViewerPersonId.Should().Be(ViewerPersonId);
        created.ViewedPersonId.Should().Be(ViewedPersonId);
        created.Reason.Should().Be("scoped grant");

        // GET list (filter by viewer)
        var list = await client.GetAsync($"/v1/visibility?viewer={ViewerPersonId:D}&active=true").ConfigureAwait(false);
        list.StatusCode.Should().Be(HttpStatusCode.OK);
        var listDoc = await list.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        listDoc.GetProperty("items").EnumerateArray().Should().ContainSingle();

        // DELETE
        var del = await client.DeleteAsync($"/v1/visibility/{created.VisibilityId:D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.NoContent);

        // Now active=true returns empty
        var list2 = await client.GetAsync($"/v1/visibility?viewer={ViewerPersonId:D}&active=true").ConfigureAwait(false);
        var list2Doc = await list2.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        list2Doc.GetProperty("items").EnumerateArray().Should().BeEmpty();
    }

    [Fact]
    public async Task Visibility_delete_unknown_id_returns_404()
    {
        var client = _adminApp!.CreateClient();
        var del = await client.DeleteAsync($"/v1/visibility/{Guid.NewGuid():D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.NotFound);
    }

    [Fact]
    public async Task Visibility_create_with_empty_viewer_returns_400()
    {
        var client = _adminApp!.CreateClient();
        var body = new CreateVisibilityCommandModel(Guid.Empty, ViewedPersonId, null, null);
        var post = await client.PostJsonAsync("/v1/visibility", body).ConfigureAwait(false);
        post.StatusCode.Should().Be(HttpStatusCode.BadRequest);
        var doc = await post.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        doc.GetProperty("type").GetString().Should().Be("urn:insight:error:invalid_viewer_person_id");
    }

    // ── /v1/roles CRUD ──────────────────────────────────────────────

    [Fact]
    public async Task Roles_list_includes_seeded_admin()
    {
        var client = _adminApp!.CreateClient();
        var resp = await client.GetAsync("/v1/roles").ConfigureAwait(false);
        resp.StatusCode.Should().Be(HttpStatusCode.OK);
        var doc = await resp.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        var items = doc.GetProperty("items").EnumerateArray()
            .Select(x => x.GetProperty("name").GetString()).ToList();
        items.Should().Contain("admin");
    }

    [Fact]
    public async Task Roles_create_and_delete_round_trip()
    {
        var client = _adminApp!.CreateClient();
        var body = new CreateRoleCommandModel("auditor");
        var post = await client.PostJsonAsync("/v1/roles", body).ConfigureAwait(false);
        post.StatusCode.Should().Be(HttpStatusCode.Created);
        var created = await post.ReadJsonAsync<RoleResponse>().ConfigureAwait(false);
        created!.Name.Should().Be("auditor");

        var del = await client.DeleteAsync($"/v1/roles/{created.RoleId:D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.NoContent);
    }

    [Fact]
    public async Task Roles_create_duplicate_name_returns_409()
    {
        var client = _adminApp!.CreateClient();
        var body = new CreateRoleCommandModel("admin"); // already seeded
        var post = await client.PostJsonAsync("/v1/roles", body).ConfigureAwait(false);
        post.StatusCode.Should().Be(HttpStatusCode.Conflict);
        var doc = await post.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        doc.GetProperty("type").GetString().Should().Be("urn:insight:error:role_name_exists");
    }

    [Fact]
    public async Task Roles_delete_in_use_returns_422_role_in_use()
    {
        // admin role has an active person_roles row (the test admin caller).
        var client = _adminApp!.CreateClient();
        var del = await client.DeleteAsync($"/v1/roles/{Roles.Admin:D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.UnprocessableEntity);
        var doc = await del.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        doc.GetProperty("type").GetString().Should().Be("urn:insight:error:role_in_use");
    }

    // ── /v1/person-roles CRUD ───────────────────────────────────────

    [Fact]
    public async Task PersonRoles_create_list_revoke_round_trip()
    {
        var client = _adminApp!.CreateClient();
        var body = new CreatePersonRoleCommandModel(NonAdminId, Roles.Admin, null, "promote");
        var post = await client.PostJsonAsync("/v1/person-roles", body).ConfigureAwait(false);
        post.StatusCode.Should().Be(HttpStatusCode.Created);
        var created = await post.ReadJsonAsync<PersonRoleResponse>().ConfigureAwait(false);
        created!.PersonId.Should().Be(NonAdminId);
        created.RoleId.Should().Be(Roles.Admin);

        var list = await client.GetAsync($"/v1/person-roles?person={NonAdminId:D}&active=true").ConfigureAwait(false);
        list.StatusCode.Should().Be(HttpStatusCode.OK);
        var listDoc = await list.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        listDoc.GetProperty("items").EnumerateArray().Should().ContainSingle();

        var del = await client.DeleteAsync($"/v1/person-roles/{created.PersonRoleId:D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.NoContent);
    }

    [Fact]
    public async Task PersonRoles_create_with_empty_person_id_returns_400()
    {
        var client = _adminApp!.CreateClient();
        var body = new CreatePersonRoleCommandModel(Guid.Empty, Roles.Admin, null, null);
        var post = await client.PostJsonAsync("/v1/person-roles", body).ConfigureAwait(false);
        post.StatusCode.Should().Be(HttpStatusCode.BadRequest);
    }

    [Fact]
    public async Task PersonRoles_revoke_last_admin_returns_422_last_admin_protected()
    {
        // The fixture seeds exactly one admin (AdminPersonId in the
        // test tenant). Find that row via the list endpoint, then try
        // to delete it — guard must refuse with 422.
        var client = _adminApp!.CreateClient();
        var list = await client.GetAsync($"/v1/person-roles?person={AdminPersonId:D}&role={Roles.Admin:D}&active=true")
            .ConfigureAwait(false);
        list.StatusCode.Should().Be(HttpStatusCode.OK);
        var listDoc = await list.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        var items = listDoc.GetProperty("items").EnumerateArray().ToArray();
        items.Should().ContainSingle("the fixture seeds exactly one admin row");
        var personRoleId = items[0].GetProperty("person_role_id").GetGuid();

        var del = await client.DeleteAsync($"/v1/person-roles/{personRoleId:D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.UnprocessableEntity);
        var doc = await del.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        doc.GetProperty("type").GetString().Should().Be("urn:insight:error:last_admin_protected");
    }

    [Fact]
    public async Task PersonRoles_revoke_admin_when_another_admin_exists_succeeds()
    {
        // Grant a second admin so the guard sees two active rows,
        // then revoke the original — guard must allow it.
        var client = _adminApp!.CreateClient();
        var grantBody = new CreatePersonRoleCommandModel(NonAdminId, Roles.Admin, null, "co-admin");
        var grant = await client.PostJsonAsync("/v1/person-roles", grantBody).ConfigureAwait(false);
        grant.StatusCode.Should().Be(HttpStatusCode.Created);

        var list = await client.GetAsync($"/v1/person-roles?person={AdminPersonId:D}&role={Roles.Admin:D}&active=true")
            .ConfigureAwait(false);
        var listDoc = await list.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        var firstAdminId = listDoc.GetProperty("items").EnumerateArray().First()
            .GetProperty("person_role_id").GetGuid();

        var del = await client.DeleteAsync($"/v1/person-roles/{firstAdminId:D}").ConfigureAwait(false);
        del.StatusCode.Should().Be(HttpStatusCode.NoContent);
    }

    [Fact]
    public async Task PersonRoles_concurrent_admin_revokes_serialise_to_one_204_one_422()
    {
        // Two admins, two parallel revokes — one targets each row, each
        // caller is the row's owner. Without the FOR UPDATE lock in
        // `TrySoftDeletePersonRoleProtectingLastAdminAsync`, the
        // correlated COUNT inside the UPDATE's derived table is a
        // snapshot read; both transactions could observe count=2 and
        // both commit, leaving the tenant with zero active admins.
        // With the lock, the second transaction blocks until the first
        // commits, then sees count=1 and refuses with 422. Outcome must
        // be exactly one 204 + one 422 in either order.
        //
        // Promote NonAdminId to admin so we have 2 admin rows. AdminPersonId
        // is already admin from InitializeAsync.
        await InsertPersonRoleAsync(NonAdminId, Roles.Admin).ConfigureAwait(false);

        var adminClient = _adminApp!.CreateClient();
        var list = await adminClient.GetAsync($"/v1/person-roles?role={Roles.Admin:D}&active=true")
            .ConfigureAwait(false);
        var listDoc = await list.ReadJsonAsync<JsonElement>().ConfigureAwait(false);
        var items = listDoc.GetProperty("items").EnumerateArray().ToArray();
        items.Should().HaveCount(2, "the test arranges exactly two admins so the race window is meaningful");

        Guid PrIdFor(Guid pid) => items.Single(x => x.GetProperty("person_id").GetGuid() == pid)
            .GetProperty("person_role_id").GetGuid();
        var adminPrId    = PrIdFor(AdminPersonId);
        var nonAdminPrId = PrIdFor(NonAdminId);

        // Issue both DELETEs concurrently. Each task uses its own
        // HttpClient (different default caller header) to keep the
        // requests independent.
        var taskA = adminClient.DeleteAsync($"/v1/person-roles/{adminPrId:D}");
        var taskB = _nonAdminApp!.CreateClient().DeleteAsync($"/v1/person-roles/{nonAdminPrId:D}");
        var results = await Task.WhenAll(taskA, taskB).ConfigureAwait(false);

        var codes = results.Select(r => r.StatusCode).OrderBy(c => (int)c).ToArray();
        codes.Should().Equal(HttpStatusCode.NoContent, HttpStatusCode.UnprocessableEntity);

        // The 204+422 pair is the test invariant — it proves the lock
        // serialised the two transactions (one revoked, one refused).
        // We deliberately skip a final list-as-admin probe: whichever
        // caller's row got revoked is no longer admin, so we cannot
        // pick a fixed client for the read without colouring the
        // assertion by which caller won the race.
    }

    // ── Seed helpers ────────────────────────────────────────────────

    private async Task InsertPersonRoleAsync(Guid personId, Guid roleId)
    {
        await using var conn = new MySqlConnection(_fixture.ConnectionString);
        await conn.OpenAsync().ConfigureAwait(false);
        const string sql = """
            INSERT INTO person_roles
                (person_role_id, insight_tenant_id, person_id, role_id,
                 valid_from, valid_to, author_person_id, reason)
            VALUES (@id, @tenant, @person, @role, '2020-01-01 00:00:00', NULL, @person, NULL)
            """;
        await using var cmd = new MySqlCommand(sql, conn);
        cmd.Parameters.AddWithValue("@id",     Guid.NewGuid().ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@tenant", TenantId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@person", personId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@role",   roleId.ToByteArray(bigEndian: true));
        await cmd.ExecuteNonQueryAsync().ConfigureAwait(false);
    }
}
