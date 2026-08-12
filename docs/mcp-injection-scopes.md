# MCP Injection Scopes

Narada sessions can receive MCP surfaces from more than one authority locus. A
surface being visible inside a site session does not mean the surface is owned
by that site. The injection scope must stay distinct from the session alias.

This doctrine names three injection scopes:

1. **Host**
   Machine-owned capability injected into one or more sessions running on that
   host.

2. **User site**
   Operator-owned Narada capability injected from the user's personal Narada
   substrate into local site sessions.

3. **Local site**
   Site-owned capability rooted in the current Narada site or workspace.

## Why This Exists

Without explicit injection scopes, the registrar and launchers are forced to
pretend every visible MCP server is local to the active site. That creates
false diagnostics:

- a host capability can be reported as missing from a site;
- a shared user-site capability can appear to belong to every bound local site;
- restart ownership becomes ambiguous;
- config mutation ownership becomes unclear;
- a session alias such as `staccato-speech` can be mistaken for site ownership.

The correct reading is: a session may bind a surface under a site-specific
alias, but the alias is only the carrier-facing name. Ownership, policy,
restart authority, and mutation authority come from the injection scope.

## Scope Definitions

### Host Injection

Host-injected surfaces belong to the machine or runtime host. Their authority is
not created by the local site, even when they are exposed inside a local site
session.

Examples:

- `speech-mcp`, because Windows SAPI voice output is a host capability.
- Host browser, window, audio, credential broker, or scheduler surfaces when
  their policy is machine-bound rather than site-bound.

Expected properties:

- authority locus: host;
- mutation locus: host-owned config or host policy;
- restart owner: host launcher or host supervisor;
- site visibility: explicit binding into selected site sessions;
- session alias may include the local site name, but ownership remains host.

Some host-injected surfaces may be catalog-declared as default injections.
That metadata selects a surface for static bindings; progressive carrier
bindings still materialize only their explicit bootstrap allowlist and expose
the remaining admitted surfaces through `mcp-loader`. Ownership and authority
are unchanged by the loading mode.

### User-Site Injection

User-site-injected surfaces belong to the operator's personal Narada substrate,
for example `C:\Users\Andrey\Narada`. They express cross-site user authority or
shared personal control-plane state.

Examples:

- cross-site surface feedback;
- user-wide task lifecycle or inbox, when intentionally shared;
- registrar or launcher catalog state owned by the user's Narada home;
- shared operator context that spans local sites.

Expected properties:

- authority locus: user Narada site;
- mutation locus: user Narada site config or state;
- restart owner: user-site launcher or carrier registration;
- site visibility: injected into local site sessions by explicit binding;
- a User-Site-bound carrier may activate exact bindings from every registered Site admitted within the User Site's authorized roots;
- each such binding retains its own Site authority locus and permissions; root reachability is eligibility evidence, not authority merging;
- local sites can observe or request through the surface, but do not own it.

### Local-Site Injection

Local-site-injected surfaces belong to the active site or workspace. Their
authority derives from the local site root and site policy.

Examples:

- local filesystem read/write surfaces for the active workspace;
- local git surface;
- site-local mailbox projection;
- site-specific Graph mail policy;
- site ops and site coherence surfaces for the active local site.

Expected properties:

- authority locus: local site root;
- mutation locus: local site config or state;
- restart owner: local site launcher or site carrier registration;
- site visibility: normally limited to that local site session;
- policy readback should name the local root and any allowed sub-roots.

## Binding Shape

A bound MCP server should carry explicit scope metadata separate from its
alias. The canonical field is `narada_scope`:

```ts
type McpInjectionScope = "host" | "user_site" | "local_site";

type McpAuthorityLocus =
  | { kind: "host"; host_id?: string }
  | { kind: "user_site"; site_root: string }
  | { kind: "local_site"; site_root: string };

type BoundMcpSurface = {
  surface_id: string;
  session_alias: string;
  narada_scope: {
    injection_scope: McpInjectionScope;
    authority_locus: McpAuthorityLocus;
    mutation_locus: McpAuthorityLocus;
    restart_owner: McpInjectionScope;
    bound_into_site?: string;
    scope_source:
      | "registrar_surface_catalog"
      | "site_config_narada_scope"
      | "site_config_legacy_top_level";
  };
};
```

## Surface Projections And Runtime Affinity

The package surface and its injected projection are separate concepts. A
surface package identifies the adapter implementation and tool contract. A
projection identifies one admitted authority boundary and, when needed, one
runtime affinity. A package may expose more than one projection, but a bound
session receives one explicitly selected projection for that package.

The registrar projection shape is:

```ts
type McpRuntimeKind = "nars";

type McpSurfaceProjection = {
  id: string;
  injection_scope: McpInjectionScope;
  default_injection?:
    | "all_site_bound_sessions"
    | "all_carrier_sessions"
    | "runtime_selected_sessions";
  runtime_requirements?: McpRuntimeKind[];
};

type MaterializedSurfaceProjection = McpSurfaceProjection & {
  projection_id: string;
  runtime_kind?: McpRuntimeKind;
};
```

Runtime affinity is an availability selector, not a permission grant. The
surface still enforces caller identity, site authority, and mutation policy.
The launcher must pass the selected runtime kind into materialization; the
registrar must not infer it from a server name, current directory, or entrypoint
path. If a surface has multiple compatible projections and neither a runtime
kind nor an explicit `projection_id` selects exactly one, materialization must
refuse rather than guess.

When a runtime kind selects a projection, the materialized server config records
that selection as `surface_projection.runtime_kind`. This is launch input
provenance, not a permission grant; the carrier loader uses it only to filter
runtime-affined surfaces for the selected runtime.

Selection precedence is explicit: a projection whose `runtime_requirements`
contains the selected runtime wins over runtime-neutral projections. If no
runtime-specific projection matches, exactly one runtime-neutral projection may
serve as the fallback; multiple neutral projections still require an explicit
`projection_id`. The loader keeps runtime-neutral servers available to every
runtime and skips only projections whose declared requirements are not
satisfied.

`nars-session-mcp` is the reference case:

- `user-site-operator` is a User Site projection for operator-facing discovery
  and governed input delivery.
- `local-site-nars-runtime` is a Local Site projection with
  `runtime_requirements: ["nars"]`. It is materialized when the launcher
  explicitly selects the NARS runtime.

Both projections use the same `@narada-core/nars-session-mcp` entrypoint. The
projection metadata, not a duplicated package or site-specific carrier profile,
defines why that entrypoint is present in a session.

For example, a Staccato session alias for speech should be understood as:

```ts
{
  surface_id: "speech",
  session_alias: "staccato-speech",
  narada_scope: {
    injection_scope: "host",
    authority_locus: { kind: "host" },
    mutation_locus: { kind: "host" },
    restart_owner: "host",
    bound_into_site: "narada-staccato",
    scope_source: "registrar_surface_catalog"
  }
}
```

The name `staccato-speech` means "host speech injected into the Staccato
session", not "speech owned by Staccato".

## Registrar And Launcher Implications

Registrar and launcher surfaces should distinguish these operations:

- **cataloging** a surface package and its tools;
- **declaring** the surface's default injection scope;
- **declaring** whether a host surface is default-injected into all site-bound
  sessions or all carrier sessions;
- **binding** the surface into a site session under an alias;
- **mutating** config at the authority locus;
- **restarting** the process owned by the restart locus;
- **diagnosing** readiness from the correct locus.

A readiness check should not mark a local site incoherent merely because a
host-level or user-site-level surface is absent from the local site's own
config. It should report that the site session lacks an expected injected
surface, and then identify the authority locus that must repair the binding.

## Compatibility And Migration Contract

The concrete compatibility contract is:

1. **Producers write `narada_scope`.**
   New registrar-generated sidecars, materialization readback, and diagnostics
   should expose the nested `narada_scope` object.

2. **Readers prefer `narada_scope`.**
   If a server record has `narada_scope`, validators and launch/readiness
   diagnostics should use it as the source of truth.

3. **Legacy top-level fields are read-only compatibility.**
   Older configs may contain `injection_scope`, `authority_locus`,
   `mutation_locus`, and `restart_owner` as top-level fields. Readers may use
   them when `narada_scope` is absent, but new writers should not introduce new
   top-level-only scope metadata.

4. **Missing scope falls back to catalog/default.**
   If neither `narada_scope` nor valid legacy top-level fields exist, readers
   fall back to the registrar surface catalog. Undeclared catalog entries
   default to `local_site`.

5. **Diagnostics report the scope source.**
   Validation findings should include `narada_scope.scope_source` so callers can
   tell whether the scope came from modern config, legacy config, or catalog
   fallback.

During migration, registrar and launcher code may infer:

- known host capabilities such as `speech` as `host`;
- known user-site control-plane capabilities as `user_site`;
- all other site-rooted surfaces as `local_site`.

Inference is a compatibility bridge, not the target state. New bindings should
declare scope through `narada_scope`, and readers should prefer that object over
flattened fields.

## V2 Projection And Lifecycle Authority

Native V2 descriptors make the scope and lifecycle boundary explicit before a carrier config is generated. A projection declares its transport, injection scope, authority requirements, runtime requirements, and lifecycle requirement. Runtime-neutral projections are eligible without a runtime kind; runtime-affined projections require an explicit matching runtime such as `nars`.

Lifecycle ownership follows the projection rather than the session alias. A replayable child may be replaced by `mcp-loader`; a session-pinned or non-replayable runtime requires the carrier or runtime supervisor. Reconciliation must report the responsible actuator and required authority, not silently apply a repair from the observing surface.

