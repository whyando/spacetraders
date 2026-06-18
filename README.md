# SpaceTraders agent

An autonomous [SpaceTraders](https://spacetraders.io) agent. It runs a fleet (trading,
mining, contracts, construction, exploration) and exposes a read-only JSON API consumed
by the standalone [`spacetraders-dashboard`](../spacetraders-dashboard) SPA.

## Configuration

Config comes from environment variables, loaded from a local `.env` file in development
(see `.env.example` for the full list). The key ones:

- `POSTGRES_URI`, `POSTGRES_SCHEMA` — TimescaleDB connection + per-reset schema slice.
- `SPACETRADERS_ACCOUNT_TOKEN`, `AGENT_CALLSIGN`, `AGENT_FACTION` — the agent to run.
- `WEB_PORT` — port for the read-only JSON API (default `8080`).

## Development

### Running locally against the dev DB

1. Start the database tunnel (port-forwards the cluster Postgres to `localhost:5432`):

   ```bash
   ./dev.sh
   ```

   `POSTGRES_URI`'s host must be `127.0.0.1` to match the tunnel — the cluster hostname
   does not resolve locally.

2. Run the agent:

   ```bash
   cargo run
   ```

   The JSON API comes up on `WEB_PORT` (default `8080`), e.g. `curl localhost:8080/api/systems`.

### Throwaway test agents

When testing against the dev DB, isolate the run so it can't pollute the live agent's
data. `agent_metrics` is keyed by timestamp only, so a second agent writing into the
**live schema** mixes its metrics into the dashboard's history chart. Use a fresh schema
and an unused callsign:

```bash
POSTGRES_SCHEMA=test_{RESET_DATE} AGENT_CALLSIGN=<unused-name> cargo run
```

Then `curl localhost:8080/api/...` to exercise the API, and drop the test schema when
done:

```sql
DROP SCHEMA <schema> CASCADE;
```

### Regenerating the Diesel schema

`src/schema.rs` is generated from `spacetraders_schema.sql.template` (the source of truth,
applied at startup via `CREATE TABLE IF NOT EXISTS`). After editing the template, run:

```bash
./generate_schema.sh
```
