using FluentAssertions;
using MySqlConnector;
using Xunit;

namespace Insight.Identity.Tests.Integration;

/// <summary>
/// Pins the schema corrections made in
/// <c>Migrations/004_persons_relax_constraints.sql</c> (ADR-0011):
///
/// * The old UNIQUE on (..., value_hash) is gone; the new one keys on
///   (..., created_at) so genuine state transitions on the same
///   partition (Active -> Inactive -> Active) persist as separate
///   rows while re-runs of the seeder at the same created_at still
///   collapse via INSERT IGNORE.
/// * `value_id` collation is `utf8mb4_unicode_ci` so all value-column
///   comparisons (email, source-native ids, parent_email, etc.) are
///   case-insensitive at the storage layer — a lookup for
///   `jane.doe@...` finds a stored `Jane.Doe@...`.
///
/// If a future migration accidentally reintroduces the value_hash
/// UNIQUE or reverts the collation, the tests below fail loudly.
/// </summary>
[Collection(MariaDbCollection.Name)]
public sealed class PersonsSchemaTests : IAsyncLifetime
{
    private static readonly Guid TenantId       = Guid.Parse("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    private static readonly Guid SourceId       = Guid.Parse("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
    private static readonly Guid PersonId       = Guid.Parse("cccccccc-cccc-cccc-cccc-cccccccccccc");
    private static readonly Guid AuthorPersonId = Guid.Empty;

    private readonly MariaDbFixture _fixture;

    public PersonsSchemaTests(MariaDbFixture fixture) => _fixture = fixture;

    public async Task InitializeAsync() => await _fixture.ResetAsync().ConfigureAwait(false);
    public Task DisposeAsync() => Task.CompletedTask;

    // ── Defect A regression: state transitions are recordable ───────

    [Fact]
    public async Task Persons_allows_state_transition_with_same_value_at_different_created_at()
    {
        // Active(T0) -> Inactive(T2) -> Active(T3) — re-activation.
        // Before ADR-0011 the second 'Active' INSERT silently dropped
        // because (tenant, person, source_type, source_id, value_type,
        // value_hash) UNIQUE collided with the first 'Active'. After
        // the migration the UNIQUE is on `created_at` instead, so all
        // three rows persist.
        var t0 = new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);
        var t2 = new DateTime(2026, 6, 1, 0, 0, 0, DateTimeKind.Utc);
        var t3 = new DateTime(2026, 8, 1, 0, 0, 0, DateTimeKind.Utc);

        await InsertStatusAsync(t0, "Active").ConfigureAwait(false);
        await InsertStatusAsync(t2, "Inactive").ConfigureAwait(false);
        await InsertStatusAsync(t3, "Active").ConfigureAwait(false);   // ← used to fail

        var statuses = await QueryStatusObservationsAsync().ConfigureAwait(false);
        statuses.Should().HaveCount(3);
        statuses[0].Should().Be((t0, "Active"));
        statuses[1].Should().Be((t2, "Inactive"));
        statuses[2].Should().Be((t3, "Active"));
    }

    [Fact]
    public async Task Persons_insert_ignore_dedupes_on_same_created_at()
    {
        // Re-running the seeder against the same source snapshot
        // produces rows with identical (tenant, person, source_type,
        // source_id, value_type, created_at). INSERT IGNORE must
        // collapse the second insert into a no-op so seed idempotency
        // survives the UNIQUE swap.
        var t0 = new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);

        await InsertStatusAsync(t0, "Active").ConfigureAwait(false);
        await InsertStatusAsync(t0, "Active").ConfigureAwait(false);  // duplicate — should noop
        await InsertStatusAsync(t0, "Active").ConfigureAwait(false);  // and again

        var statuses = await QueryStatusObservationsAsync().ConfigureAwait(false);
        statuses.Should().HaveCount(1);
    }

    // ── Defect B regression: value_id comparison is case-insensitive ──

    [Fact]
    public async Task Persons_value_id_comparison_is_case_insensitive_for_emails()
    {
        // Store the email with mixed case (as BambooHR may emit it),
        // then look it up lowercase. With utf8mb4_unicode_ci, the
        // comparison treats them as equal.
        await InsertEmailAsync("Jane.Doe@example.com").ConfigureAwait(false);

        var personIdLowercase = await ResolveByEmailAsync("jane.doe@example.com").ConfigureAwait(false);
        var personIdMixedCase = await ResolveByEmailAsync("JANE.DOE@EXAMPLE.COM").ConfigureAwait(false);
        var personIdOriginal  = await ResolveByEmailAsync("Jane.Doe@example.com").ConfigureAwait(false);

        personIdLowercase.Should().Be(PersonId);
        personIdMixedCase.Should().Be(PersonId);
        personIdOriginal.Should().Be(PersonId);
    }

    [Fact]
    public async Task Persons_value_id_comparison_is_case_insensitive_for_source_native_ids()
    {
        // The collation switch applies to ALL value_id-routed types,
        // not just email. UUID-shaped parent_person_id stored in
        // mixed case (in practice all-lowercase, but defensively
        // testing) is found by an upper-case query.
        await InsertParentPersonIdAsync("019e166b-6FBD-768B-919C-95E218229A64").ConfigureAwait(false);

        var found = await ResolveByParentPersonIdAsync("019e166b-6fbd-768b-919c-95e218229a64").ConfigureAwait(false);
        found.Should().Be(PersonId);
    }

    // ── Schema introspection: pin the constraint shape ─────────────

    [Fact]
    public async Task Persons_schema_unique_observation_keys_on_created_at_not_value_hash()
    {
        // Pin the new UNIQUE shape so a future migration that
        // accidentally re-adds value_hash trips here.
        await using var conn = new MySqlConnection(_fixture.ConnectionString);
        await conn.OpenAsync().ConfigureAwait(false);

        const string sql = """
            SELECT COLUMN_NAME, SEQ_IN_INDEX
            FROM information_schema.STATISTICS
            WHERE TABLE_SCHEMA = DATABASE()
              AND TABLE_NAME   = 'persons'
              AND INDEX_NAME   = 'uq_person_observation'
            ORDER BY SEQ_IN_INDEX
            """;
        await using var cmd = new MySqlCommand(sql, conn);
        await using var reader = await cmd.ExecuteReaderAsync().ConfigureAwait(false);

        var cols = new List<string>();
        while (await reader.ReadAsync().ConfigureAwait(false))
            cols.Add(reader.GetString("COLUMN_NAME"));

        cols.Should().ContainInOrder(
            "insight_tenant_id", "person_id",
            "insight_source_type", "insight_source_id",
            "value_type", "created_at");
        cols.Should().NotContain("value_hash");
    }

    [Fact]
    public async Task Persons_value_id_column_uses_unicode_ci_collation()
    {
        // Pin the collation so a future migration that reverts to
        // utf8mb4_bin trips here.
        await using var conn = new MySqlConnection(_fixture.ConnectionString);
        await conn.OpenAsync().ConfigureAwait(false);

        const string sql = """
            SELECT COLLATION_NAME
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = DATABASE()
              AND TABLE_NAME   = 'persons'
              AND COLUMN_NAME  = 'value_id'
            """;
        await using var cmd = new MySqlCommand(sql, conn);
        var collation = (string?)await cmd.ExecuteScalarAsync().ConfigureAwait(false);

        collation.Should().Be("utf8mb4_unicode_ci");
    }

    // ── Helpers ────────────────────────────────────────────────────────

    private async Task InsertStatusAsync(DateTime createdAt, string statusValue)
        => await InsertObservationAsync(
            createdAt, valueType: "status",
            valueId: null, valueFullText: statusValue, value: null
        ).ConfigureAwait(false);

    private async Task InsertEmailAsync(string emailValue)
        => await InsertObservationAsync(
            createdAt: new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc),
            valueType: "email",
            valueId: emailValue, valueFullText: null, value: null
        ).ConfigureAwait(false);

    private async Task InsertParentPersonIdAsync(string parentPersonIdText)
        => await InsertObservationAsync(
            createdAt: new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc),
            valueType: "parent_person_id",
            valueId: parentPersonIdText, valueFullText: null, value: null
        ).ConfigureAwait(false);

    private async Task InsertObservationAsync(
        DateTime createdAt,
        string valueType,
        string? valueId,
        string? valueFullText,
        string? value)
    {
        await using var conn = new MySqlConnection(_fixture.ConnectionString);
        await conn.OpenAsync().ConfigureAwait(false);
        // INSERT IGNORE because the new UNIQUE on created_at silently
        // dedupes re-emits at the same timestamp — matches what the
        // seeder uses in step 7.
        const string sql = """
            INSERT IGNORE INTO persons
                (value_type, insight_source_type, insight_source_id, insight_tenant_id,
                 value_id, value_full_text, value,
                 person_id, author_person_id, reason, created_at)
            VALUES
                (@vt, 'bamboohr', @src, @tenant,
                 @vid, @vft, @vraw,
                 @person, @author, '', @ts)
            """;
        await using var cmd = new MySqlCommand(sql, conn);
        cmd.Parameters.AddWithValue("@vt",     valueType);
        cmd.Parameters.AddWithValue("@src",    SourceId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@tenant", TenantId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@vid",    (object?)valueId       ?? DBNull.Value);
        cmd.Parameters.AddWithValue("@vft",    (object?)valueFullText ?? DBNull.Value);
        cmd.Parameters.AddWithValue("@vraw",   (object?)value         ?? DBNull.Value);
        cmd.Parameters.AddWithValue("@person", PersonId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@author", AuthorPersonId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@ts",     createdAt);
        await cmd.ExecuteNonQueryAsync().ConfigureAwait(false);
    }

    private async Task<List<(DateTime CreatedAt, string Status)>> QueryStatusObservationsAsync()
    {
        await using var conn = new MySqlConnection(_fixture.ConnectionString);
        await conn.OpenAsync().ConfigureAwait(false);
        const string sql = """
            SELECT created_at, value_full_text
            FROM persons
            WHERE insight_tenant_id = @tenant
              AND person_id         = @person
              AND value_type        = 'status'
            ORDER BY created_at
            """;
        await using var cmd = new MySqlCommand(sql, conn);
        cmd.Parameters.AddWithValue("@tenant", TenantId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@person", PersonId.ToByteArray(bigEndian: true));
        await using var reader = await cmd.ExecuteReaderAsync().ConfigureAwait(false);
        var result = new List<(DateTime, string)>();
        while (await reader.ReadAsync().ConfigureAwait(false))
            result.Add((reader.GetDateTime("created_at"), reader.GetString("value_full_text")));
        return result;
    }

    private async Task<Guid?> ResolveByEmailAsync(string email)
        => await ResolveByValueIdAsync("email", email).ConfigureAwait(false);

    private async Task<Guid?> ResolveByParentPersonIdAsync(string parentPersonIdText)
        => await ResolveByValueIdAsync("parent_person_id", parentPersonIdText).ConfigureAwait(false);

    private async Task<Guid?> ResolveByValueIdAsync(string valueType, string queryValue)
    {
        await using var conn = new MySqlConnection(_fixture.ConnectionString);
        await conn.OpenAsync().ConfigureAwait(false);
        // Plain `value_id = @x` comparison — relies on the new
        // utf8mb4_unicode_ci collation for case-insensitivity.
        const string sql = """
            SELECT person_id FROM persons
            WHERE insight_tenant_id = @tenant
              AND value_type        = @vt
              AND value_id          = @q
            LIMIT 1
            """;
        await using var cmd = new MySqlCommand(sql, conn);
        cmd.Parameters.AddWithValue("@tenant", TenantId.ToByteArray(bigEndian: true));
        cmd.Parameters.AddWithValue("@vt",     valueType);
        cmd.Parameters.AddWithValue("@q",      queryValue);
        var raw = await cmd.ExecuteScalarAsync().ConfigureAwait(false);
        return raw is byte[] bytes && bytes.Length == 16
            ? new Guid(bytes, bigEndian: true)
            : null;
    }
}
