//! Cloudflare, as one module and one `register` call.
//!
//! P1-001's evidence made a claim about itself: *"P1-002 adds Cloudflare as one
//! module and one register call, with nothing changing in `apex-agent-core`,
//! `apex-agentd` or the CLI."* This directory is the test of it. Nothing in
//! those three crates knows the word Cloudflare, the wire carries an operation
//! id and an option map that were already there, and `apex secret capabilities`
//! prints these operations because the daemon advertises its registry rather
//! than because anyone added them to a list.
//!
//! ## What this supplies, and what it therefore cannot get wrong
//!
//! The four things [`crate::provider`] asks for:
//!
//! * **which operations exist** — [`SPEC`], §13.2's names for the Workers
//!   surface plus the account read that `apex cf status` is built on;
//! * **what a resource name means** — [`binding`], which is §13.1: `project`
//!   is a worker this project bound to an environment, `example.com` is its
//!   zone, and a name it did not bind does not resolve at all;
//! * **how a credential is presented** — [`api`], a `curl` the broker owns,
//!   with the token on the child's stdin and never in `argv`;
//! * **how to mint a short-lived one** — [`temporary`], which is §13.4: the
//!   stored token buys a token carrying one permission group at one scope,
//!   expiring in minutes, deleted the moment the operation returns. A request
//!   for one has four answers and they are not the same answer —
//!   [`crate::provider::Minted`] separates *there is nothing narrower* from
//!   *the account refused* from *the attempt did not run* — and the trail
//!   records which. Three of the four carry on with the stored credential,
//!   because §13.4 says *prefer*; a project that wrote
//!   `temporary_credentials = "require"` gets a refusal instead.
//!
//! And a fifth thing, which is §13.4's other sentence:
//!
//! * **how to run the tool instead of the API** — [`tools`], a broker-owned
//!   `wrangler` or `terraform` with the credential in its environment and a
//!   fixed argv this build writes.
//!
//! Everything else is the framework's, in the order [`crate::provider`] sets
//! out, and this module could not skip a step if it tried: it never sees the
//! stored value until after the grant and the host pin, and the value it does
//! see is scrubbed out of the result by somebody else.
//!
//! ## The pin is real here in a way it is not for git
//!
//! `git`'s endpoint comes out of the caller's own repository, so pinning it
//! catches a repository that points somewhere else. Cloudflare's endpoint is
//! [`api::API_HOST`], a constant. That makes the pin a flat statement: a
//! credential stored for any other host cannot be spent on a Cloudflare
//! operation, and a Cloudflare token stored for `api.cloudflare.com` cannot be
//! spent anywhere else. Neither half needs this module's cooperation.
//!
//! ## Resolving twice, and why
//!
//! [`crate::provider::Bound`] carries an endpoint and a sentence, and nothing a
//! provider defines. So what `bind` worked out cannot be handed to `perform`,
//! and `perform` has to read the project file again. Between the two reads the
//! owner could change it — the owner is the caller — so `perform` rebuilds the
//! sentence and refuses if it is not the one `bind` was pinned on. That closes
//! it here. It is not closed in general, and the git provider has the same gap:
//! `bind` resolves a remote with `git remote get-url` and `perform` runs
//! `git push <remote>`, which resolves it again inside the child.
//!
//! ## What R2 can and cannot carry
//!
//! §13.5 asks for bucket- and object-scoped capabilities, and that is what the
//! three R2 operations are: the bucket is one the project's own file lists, and
//! an object is a key under it. Three limits are in the build rather than in a
//! plan, and each is a consequence of something else here being right:
//!
//! * **an object comes back as text.** [`crate::broker::run_curl`] hands back a
//!   `String`, and every reply in this service travels to the caller as one. A
//!   read of a PNG therefore arrives lossily converted. Text objects — a SQL
//!   dump, a JSON manifest, a log — are exact, and those are what an agent has
//!   any business reading through a broker;
//! * **an object key is a [`apex_secret_core::operation::Syntax::Path`]**, so
//!   `backups/2026-09-12.sql` can be named and `backups/2026-09-12T10:00.sql`
//!   cannot: `:` is refused by the grammar because a resource that could carry
//!   one could be read as a URL. The grammar is load-bearing and the key
//!   restriction is the price;
//! * **ten megabytes**, [`MAX_PAYLOAD`], against the documented endpoint's 300.
//!   A backup larger than that is not an agent operation.
//!
//! §13.5 also calls R2 "the preferred first-party cloud target for encrypted
//! APEX backups". That is P1-010's, and the object write below is the half of
//! it that does not need a backup format to exist first.
//!
//! ## The four storage surfaces, and the one thing they share
//!
//! D1, KV, Queues and Hyperdrive are §13.3's, and every one of them is
//! addressed by an **id** — there is no name-based path for any of the four,
//! the way there is for an R2 bucket. So §13.1's file carries the name the
//! project uses and the id the API needs, one table per product, and a name it
//! does not list does not resolve. A build that took the id from the caller
//! would have no binding at all: "which database" would be the agent's answer
//! rather than the owner's.
//!
//! Three consequences worth stating where they will be read:
//!
//! * **a D1 database id is a UUID and the other three are 32 hex.** That is
//!   Cloudflare's split, not a convenience, and one validator for both would
//!   either refuse every real D1 id or admit 36 characters of anything into a
//!   URL path;
//! * **a KV read answers with the value's bytes**, like an R2 object read, so
//!   the same lossy-text limitation applies to a value that is not UTF-8;
//! * **a Hyperdrive configuration carries the password of the database it
//!   fronts.** `edit` declares a name and three caching settings and nothing
//!   else, so the framework refuses an origin field before this module is
//!   asked; and `read` has its reply stripped of `password` and
//!   `access_client_secret` even though the documentation says they are
//!   write-only and never returned. A guarantee that an agent gets
//!   capabilities and not credentials cannot rest on a remark in somebody
//!   else's documentation staying true.
//!
//! ## §13.11, and a finding that decided its shape
//!
//! An AI Gateway run looked like it would need a second host. The
//! provider-specific endpoint is `gateway.ai.cloudflare.com`, the framework
//! pins a credential to the host it was stored for, and that would have meant
//! the owner storing the same token twice under two service names.
//!
//! It does not. AI Gateway's REST API page documents the **gatewayed form of
//! the ordinary call** — `POST /accounts/{id}/ai/run/{model}` with a
//! `cf-aig-gateway-id` header, on `api.cloudflare.com` like everything else
//! here — so `workers-ai.run` and `ai-gateway.run` are the same URL and the
//! same credential, and what separates them is a header and a grant. The pin
//! never has to be argued with.
//!
//! **Where that comes from, and where it does not.** Every other claim in this
//! provider is checked against the pinned schema. This one cannot be: the
//! schema's `workers-ai-post-run-model` declares two path parameters and **no
//! headers at all**, and the string `cf-aig` does not occur anywhere in its
//! 25MB. So the schema neither documents this nor contradicts it, and the
//! source is the documentation:
//!
//! * <https://developers.cloudflare.com/ai-gateway/usage/rest-api/> shows the
//!   gatewayed call with the model in the URL path and the `cf-aig-gateway-id`
//!   header, and says in as many words that "the existing Workers AI endpoint
//!   with the model ID in the URL path also continues to work";
//! * <https://developers.cloudflare.com/workers-ai/get-started/rest-api/> shows
//!   the ungatewayed one — the same path, `Authorization` only, a `prompt`
//!   body — which is what `workers-ai.run` sends;
//! * <https://developers.cloudflare.com/ai-gateway/configuration/custom-metadata/>
//!   is where `cf-aig-metadata`'s limits come from: at most five entries,
//!   values of string, number or boolean, and keys beginning `cf.` reserved to
//!   Cloudflare. [`usage_metadata`] sends three strings and a test asserts all
//!   three limits.
//!
//! One line on that first page reads "Workers AI requests always require this
//! header", which looks like it contradicts the second page. It does not: that
//! sentence is about AI Gateway's own front door, `POST /ai/run` with the model
//! in the **body**. This build uses the path form for both operations, which
//! that same page says keeps working. If it ever stops,
//! `a_gatewayed_run_is_the_same_host_and_the_same_credential_as_an_ungatewayed_one`
//! is the test that describes what was assumed.
//!
//! Two consequences worth stating where they will be read:
//!
//! * **the gateway id comes from §13.1's file and never from the caller.**
//!   Cloudflare creates a gateway on the first authenticated request that names
//!   one that does not exist, so a caller that could name a gateway could
//!   create one — and bill it;
//! * **the model is bound too.** `@cf/meta/llama-3.1-8b-instruct` is not an
//!   [`apex_secret_core::operation::Syntax::Name`] — the grammar wants an
//!   alphanumeric first character — so a model could not be named as a resource
//!   even if this build wanted the agent to choose one. §13.14's cost
//!   guardrails start here, with the owner deciding which models a project may
//!   run.
//!
//! §13.11's remainder is **not** here and cannot be: *"APEX local AI service
//! may route to… Workers AI, AI Gateway"*. `apex-aid` is that service, and two
//! things about it, both checked rather than assumed:
//!
//! * `apexd_core::ai::Backend` is a *compute* backend — `Cuda`, `Rocm`,
//!   `Vulkan`, `Cpu` — with no notion of a remote provider to select. Named
//!   here in plain backticks and not as an intra-doc link on purpose:
//!   `apexd-core` is not a dependency of this crate, which is itself part of
//!   the point, and a link to it would be a rustdoc warning rather than a link;
//! * the inference runtime is started under `bubblewrap` with `--unshare-net`,
//!   and `apex-aid`'s own module note calls that flag the load-bearing one.
//!   The backend cannot reach `api.cloudflare.com` because it cannot reach
//!   anything.
//!
//! So routing the local service through Cloudflare is not a missing URL, it is
//! a design change in another crate: something outside the sandbox would have
//! to make the brokered call and hand the answer in. What this provider
//! supplies is that brokered route, which is the half that belongs here.
//!
//! ## §13.10, and the one thing this surface does that no other does
//!
//! Access and Tunnels are §13.10's, and they are the first operations here that
//! can **create a credential**. `POST /access/service_tokens` answers with a
//! `client_secret`; Cloudflare shows it once. §13.10 says where it goes —
//! *"directly into protected storage… the agent receives handles/capabilities,
//! not plaintext secrets"* — and [`crate::provider::Bound::creates`] is how it
//! gets there: this module names the service before the call so the framework
//! can refuse a collision while refusing is free, hands back the value after,
//! and returns the `client_id` and the handle to the caller.
//!
//! The pin is the part worth reading twice. A service token is presented to the
//! application Access guards, never to the API that issued it, so the stored
//! credential is pinned to a **host in this project's own zone** — resolved
//! through [`binding::Binding::record`], with §13.9's label boundary and its
//! `records` narrowing. A project that narrowed itself to some names cannot pin
//! a credential to a different one, and a credential pinned to
//! `api.cloudflare.com` would be a pin that says the wrong thing.
//!
//! ## What has never run against Cloudflare
//!
//! All of it. There is no account and no token on this machine, so every
//! operation below has been exercised against a loopback server that speaks the
//! same envelope and refuses without an `Authorization` header, and none of it
//! against `api.cloudflare.com`. The paths are the documented ones — read out
//! of `cloudflare/api-schemas`' `openapi.json` at `d5003a19`, and cross-checked
//! against the rendered reference — and that they are the documented ones is
//! not the same as having called them.

pub mod api;
pub mod binding;
pub mod deploy;
pub mod dns;
pub mod temporary;
pub mod tools;

use apex_secret_core::operation::{
    self, Effect, OperationSpec, ParamSpec, ProviderSpec, ResourceKind, Syntax,
};
use apex_secret_core::project::{self, MAX_PAYLOAD};
use apex_secret_core::SecretValue;

use crate::provider::{Approval, Bind, Bound, Endpoint, Lease, Minted, Performed, Provider, ProviderError};

use api::{Api, Body, Call, Multipart};
use binding::{Account, Binding, BindingError, Bucket, Narrowing, Protection, Resource, Worker, Zone};
use dns::{Lookup, Record};
use tools::Tools;
use temporary::Scope;

/// The worker, zone or bucket a caller names.
const NAMED: ResourceKind = ResourceKind::Name;

/// `message`, the annotation Cloudflare stores against a version or a
/// deployment. `Text`, so it never reaches a command line — it goes in a JSON
/// body this module builds.
const MESSAGE: ParamSpec = ParamSpec {
    name: "message",
    syntax: Syntax::Text,
    required: false,
    summary: "a line recorded with it, for whoever reads the history later",
};

/// `version`, a Worker version id.
const VERSION: ParamSpec = ParamSpec {
    name: "version",
    syntax: Syntax::Name,
    required: true,
    summary: "the version id to put in front of traffic",
};

/// `percentage`, §13.7's staged rollout.
///
/// Optional, and absent means all of it — which is what this operation did
/// before there was a choice, so a caller who never names it sees no change.
///
/// `Syntax::Name` because a share is digits and at most one `.`, and `Name`
/// already refuses a leading `-`: a negative share would otherwise reach the
/// parser as a number the far side would have to reject. The range is checked
/// in [`deploy::share`] against the one the schema documents, and a value
/// outside it is refused rather than clamped.
const SHARE: ParamSpec = ParamSpec {
    name: "percentage",
    syntax: Syntax::Name,
    required: false,
    summary: "how much traffic this version takes, 0.01 to 100; the rest stays \
              on the version that has it now",
};

/// The bucket, database, namespace or object a caller names as a path within
/// something the project bound. `example-assets/backups/db.sql`.
const WITHIN: ResourceKind = ResourceKind::Path;

/// `file`, a path inside the project whose bytes are the request.
///
/// The counterpart to §13.4's rule about credentials, for payloads: the bytes
/// an agent uploads come out of the project it is working in, read by the
/// daemon under [`project::read_file`]'s rules, rather than travelling through
/// the protocol as a parameter. A `Syntax::Path` cannot name `/etc/shadow`,
/// and `read_file` would refuse it anyway — it is not the caller's file.
const FILE: ParamSpec = ParamSpec {
    name: "file",
    syntax: Syntax::Path,
    required: true,
    summary: "the file inside the project whose bytes to send",
};

/// `type`, a DNS record type. Upper-cased and checked against
/// [`dns::TYPES`], after [`dns::ELEVATED`] has had its say.
const KIND: ParamSpec = ParamSpec {
    name: "type",
    syntax: Syntax::Name,
    required: true,
    summary: "the record type: A, AAAA, CNAME, TXT, MX and so on",
};

/// `content`, what a record points at.
///
/// `Text` because a TXT record's value is free-form and an SPF line has
/// spaces in it. It goes in a JSON body this module builds, which is what
/// [`Syntax::Text`] requires of whoever declares one.
const CONTENT: ParamSpec = ParamSpec {
    name: "content",
    syntax: Syntax::Text,
    required: true,
    summary: "what the record points at — an address, a name, a text value",
};

/// The fields a record carries besides its content, on create and on update.
const TTL: ParamSpec = ParamSpec {
    name: "ttl",
    syntax: Syntax::Name,
    required: false,
    summary: "seconds to cache it for, or 1 for automatic",
};

const PROXIED: ParamSpec = ParamSpec {
    name: "proxied",
    syntax: Syntax::Name,
    required: false,
    summary: "true to serve it through Cloudflare, false to answer with the origin",
};

const COMMENT: ParamSpec = ParamSpec {
    name: "comment",
    syntax: Syntax::Text,
    required: false,
    summary: "a line stored with the record, for whoever reads the zone later",
};

/// `prompt`, one line for a model to answer.
///
/// `Text`, so it is one line and it goes in a JSON body this module builds. A
/// conversation — several messages, several lines — is [`INPUT_FILE`], for the
/// same reason a D1 migration is a file and a query is a line.
const PROMPT: ParamSpec = ParamSpec {
    name: "prompt",
    syntax: Syntax::Text,
    required: false,
    summary: "what to ask the model, as one line",
};

/// `file`, a JSON document in the project to send as the request.
const INPUT_FILE: ParamSpec = ParamSpec {
    name: "file",
    syntax: Syntax::Path,
    required: false,
    summary: "a .json file inside the project to send instead — messages, \
              parameters, whatever the model takes",
};

/// `scopes`, which Cloudflare services may read a stored secret.
///
/// `Text` rather than `Name` because it is a list and a comma is not a `Name`
/// character. Each entry is checked against Cloudflare's own closed set before
/// the credential is spent.
const SCOPES: ParamSpec = ParamSpec {
    name: "scopes",
    syntax: Syntax::Text,
    required: true,
    summary: "which services may read it: workers, ai_gateway, dex, access, \
              containers or websearch, comma separated",
};

/// `comment`, a line stored beside a secret.
const COMMENT_ON_SECRET: ParamSpec = ParamSpec {
    name: "comment",
    syntax: Syntax::Text,
    required: false,
    summary: "a line stored with it, for whoever reads the store later",
};

/// `file`, the project file holding a secret's value.
///
/// The alternative to the request body, and not a third way of sending one: a
/// value has to arrive as bytes, and these are the only two byte-shaped inputs
/// an operation has. Optional here because the body is the other half.
const VALUE_FILE: ParamSpec = ParamSpec {
    name: "file",
    syntax: Syntax::Path,
    required: false,
    summary: "a file inside the project holding the value, if it is not being \
              piped in",
};

/// `sql`, one statement to run against a D1 database.
///
/// `Text`, which is one line — so this carries a statement and not a script. A
/// migration is several statements over several lines, and that is
/// `cloudflare.d1.migrate`, which reads them out of a file in the project
/// instead. The two are separate §13.2 names for that reason as much as for the
/// grant.
const SQL: ParamSpec = ParamSpec {
    name: "sql",
    syntax: Syntax::Text,
    required: true,
    summary: "the statement to run, as one line",
};

/// The vocabulary, in §13.2's shape.
///
/// Thirty-four names: **all thirty-two of §13.2's**, plus `worker.route.read`
/// and `access.service-token.create`, which are §13.3's "Worker routes" and
/// "service tokens" rather than §13.2's examples. §13.2's list is complete as
/// of P1-009; what is left of §13 is behaviour rather than vocabulary — §13.4's
/// scoped credentials, §13.7's staged rollout, §13.13's preview environments
/// and §13.14's budgets.
///
/// Declaring one this module cannot perform would put it in
/// `apex secret capabilities`, let an owner grant it, and then fail at use
/// time, which is a worse answer than not offering it.
pub const SPEC: ProviderSpec = ProviderSpec {
    id: "cloudflare",
    summary: "deploy and inspect this project's own Cloudflare workers",
    operations: &[
        OperationSpec {
            id: "cloudflare.account.read",
            summary: "read the Cloudflare account this credential belongs to",
            effect: Effect::Read,
            resource: ResourceKind::None,
            params: &[],
            aliases: &[],
            // **The declaration that forced `same_everywhere` to be a field.**
            //
            // This operation names nothing — no resource, no parameters — and
            // it still resolves against the directory the caller is standing
            // in: `resolve` reads the project's own `apex.toml`, and answers
            // `GET /accounts/{id}` for the account that file binds, or
            // `GET /accounts` for every account the token can see when it
            // binds none. Two projects, two different requests, one stored
            // token. So `*` is refused, and `apex cf status` needs a grant in
            // the project it is run in.
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.worker.read",
            summary: "read the settings of one of this project's workers",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            // Every other Cloudflare operation names a worker, which the
            // project's own `apex.toml` maps to an account and an
            // environment. Per project by construction.
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.worker.upload-version",
            summary: "upload a new version of one of this project's workers, \
                      without putting it in front of traffic",
            effect: Effect::Write,
            resource: NAMED,
            params: &[
                ParamSpec {
                    name: "script",
                    syntax: Syntax::Path,
                    required: true,
                    summary: "the module to upload, as a path inside the project",
                },
                ParamSpec {
                    name: "compatibility-date",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "the runtime date this version is written against",
                },
                MESSAGE,
            ],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.worker.deploy",
            summary: "put an uploaded version of one of this project's workers \
                      in front of its traffic, all of it or a share",
            effect: Effect::Write,
            resource: NAMED,
            params: &[VERSION, SHARE, MESSAGE],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.worker.rollback",
            summary: "put an earlier version of one of this project's workers \
                      back in front of traffic",
            effect: Effect::Write,
            resource: NAMED,
            params: &[VERSION, MESSAGE],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.worker.tail",
            summary: "start a log session for one of this project's workers",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.worker.route.read",
            summary: "read the routes this project's zone sends to its workers",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        // ── §13.3's storage surfaces: D1, KV, Queues, Hyperdrive ────────────
        //
        // Nine names, four products, and one thing in common: every one of them
        // is addressed by an id, and the id is in the project's own file. None
        // of these has a name-based path the way an R2 bucket does, so a build
        // that let the caller give the id would have no binding left — "which
        // database" would be the agent's answer rather than the owner's.
        OperationSpec {
            id: "cloudflare.d1.read",
            summary: "read the size, tables and settings of one of this \
                      project's D1 databases",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.d1.query",
            summary: "run one SQL statement against one of this project's D1 \
                      databases",
            // **Write, and not a judgement about the statement.** A build that
            // read the SQL and called `SELECT` a read would be wrong the first
            // time somebody wrote `WITH x AS (DELETE …) SELECT …`, and wrong in
            // the direction that matters. The owner grants the ability to run a
            // statement; what the statement does is the statement's.
            effect: Effect::Write,
            resource: NAMED,
            params: &[SQL],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.d1.migrate",
            summary: "apply a .sql migration file from this project to one of \
                      its D1 databases",
            effect: Effect::Write,
            resource: NAMED,
            params: &[ParamSpec {
                summary: "the .sql file inside the project to apply",
                ..FILE
            }],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.kv.read",
            summary: "read one key out of one of this project's KV namespaces",
            effect: Effect::Read,
            resource: WITHIN,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.kv.write",
            summary: "write one key in one of this project's KV namespaces",
            effect: Effect::Write,
            resource: WITHIN,
            params: &[
                ParamSpec {
                    name: "value",
                    syntax: Syntax::Text,
                    required: false,
                    summary: "the value to store, as one line",
                },
                ParamSpec {
                    required: false,
                    summary: "a file inside the project whose bytes to store instead",
                    ..FILE
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.queue.publish",
            summary: "put one message on one of this project's queues",
            effect: Effect::Write,
            resource: NAMED,
            params: &[ParamSpec {
                name: "message",
                syntax: Syntax::Text,
                required: true,
                summary: "the message to send",
            }],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.queue.manage",
            // What it is, and — because "manage" could mean anything — what it
            // is not. It cannot create a queue, delete one, rename one, or
            // change who consumes it. Those are not declared, so they cannot be
            // granted, and an owner reading this line is told as much.
            summary: "change how one of this project's queues delivers: pause \
                      it, delay it, or set how long it keeps a message",
            effect: Effect::Write,
            resource: NAMED,
            params: &[
                ParamSpec {
                    name: "paused",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "true to stop delivering, false to start again",
                },
                ParamSpec {
                    name: "delivery-delay",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "seconds to hold a message before delivering it, 0 to 86400",
                },
                ParamSpec {
                    name: "retention",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "seconds to keep an undelivered message, 60 to 1209600",
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.hyperdrive.read",
            summary: "read the settings of one of this project's Hyperdrive \
                      configurations",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.hyperdrive.edit",
            // **The origin's credentials are deliberately not here.** A
            // Hyperdrive configuration carries the password of the database it
            // fronts, and an `edit` that took one would have an agent handing a
            // credential *to* the broker — the exact inverse of what this
            // service is for. The framework refuses an option no operation
            // declares, so declaring only these four is what closes it.
            summary: "change the name or the caching of one of this project's \
                      Hyperdrive configurations — never its origin or its \
                      credentials",
            effect: Effect::Write,
            resource: NAMED,
            params: &[
                ParamSpec {
                    name: "name",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "what to call it",
                },
                ParamSpec {
                    name: "caching",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "on or off",
                },
                ParamSpec {
                    name: "max-age",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "seconds to cache a query result for",
                },
                ParamSpec {
                    name: "stale-while-revalidate",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "seconds a stale result may still be served while it refreshes",
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },

        // ── §13.5, R2 ───────────────────────────────────────────────────────
        OperationSpec {
            id: "cloudflare.r2.object.read",
            summary: "read one object out of one of this project's R2 buckets, \
                      or list what is in it",
            effect: Effect::Read,
            // `example-assets` lists that bucket; `example-assets/db/today.sql`
            // reads that object. One operation and not two, because §13.2
            // names one and because the grant an owner gives is the same
            // either way: this project's buckets, read.
            resource: WITHIN,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.r2.object.write",
            summary: "put a file from this project into one of its R2 buckets",
            effect: Effect::Write,
            resource: WITHIN,
            params: &[FILE],
            aliases: &[],
            same_everywhere: false,
        },
        // ── §13.9, DNS ──────────────────────────────────────────────────────
        //
        // Four verbs and not one `dns.write`, which is §13.2's whole argument
        // in miniature: an agent that may point a preview hostname at a new
        // worker needs `update`, and giving it `delete` at the same time is a
        // different decision that an owner should get to make separately.
        OperationSpec {
            id: "cloudflare.dns.read",
            summary: "read the DNS records at one name in this project's zone",
            effect: Effect::Read,
            resource: NAMED,
            params: &[ParamSpec {
                required: false,
                ..KIND
            }],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.dns.create",
            summary: "add a DNS record at one name in this project's zone",
            effect: Effect::Write,
            resource: NAMED,
            params: &[KIND, CONTENT, TTL, PROXIED, COMMENT],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.dns.update",
            summary: "change the DNS record at one name in this project's zone",
            effect: Effect::Write,
            resource: NAMED,
            params: &[
                KIND,
                ParamSpec {
                    required: false,
                    ..CONTENT
                },
                TTL,
                PROXIED,
                COMMENT,
            ],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.dns.delete",
            summary: "remove the DNS record at one name in this project's zone",
            effect: Effect::Write,
            resource: NAMED,
            params: &[KIND],
            aliases: &[],
            same_everywhere: false,
        },
        // ── §13.11, Workers AI and the AI Gateway ───────────────────────────
        //
        // §13.11 says the APEX AI service "may route to" Workers AI and the AI
        // Gateway, and §13.14 wants what that costs attributable to a task.
        // Three things about the shape, because two of them are the opposite of
        // what the obvious reading suggests:
        //
        // * **an AI Gateway run is the same host and the same credential.** The
        //   provider-specific endpoint at `gateway.ai.cloudflare.com` exists,
        //   but Cloudflare's REST API page documents the gatewayed form of the
        //   ordinary call: `POST /accounts/{id}/ai/run/{model}` with a
        //   `cf-aig-gateway-id` header. So there is no second host, no second
        //   stored credential and no pin to argue with — the difference between
        //   these two operations is a header and a grant;
        // * **the gateway comes from the project's file and never the caller.**
        //   Cloudflare creates a gateway on first use if the id does not exist,
        //   so a caller that could name one could create one by typo — and
        //   bill it;
        // * **the model is bound too**, and not only for §13.14's guardrails:
        //   `@cf/meta/llama-3.1-8b-instruct` is not a `Syntax::Name`, so a
        //   model could not be a resource even if this build wanted it to be.
        OperationSpec {
            id: "cloudflare.workers-ai.run",
            summary: "run one of the Workers AI models this project binds",
            // **Write, and not a judgement about inference.** It costs money
            // and it is not repeatable, which is what `Effect` is for. A build
            // that called it a read because nothing is stored would let a
            // `Read` grant spend an account's balance.
            effect: Effect::Write,
            resource: NAMED,
            params: &[PROMPT, INPUT_FILE],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.ai-gateway.run",
            summary: "run one of this project's models through one of its AI \
                      gateways, with the request tagged for this task",
            effect: Effect::Write,
            // `<gateway>/<model>`: both bound, because both cost money.
            resource: WITHIN,
            params: &[PROMPT, INPUT_FILE],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.ai-gateway.edit",
            // What it changes, and what it will not touch. `PUT` REPLACES a
            // gateway, so this is a read-modify-write — and it refuses outright
            // when the gateway carries an exporter credential, because a
            // replace it cannot faithfully reproduce is a replace that would
            // delete one.
            summary: "change the caching, rate limiting and log collection of \
                      one of this project's AI gateways",
            effect: Effect::Write,
            resource: NAMED,
            params: &[
                ParamSpec {
                    name: "cache-ttl",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "seconds to cache a response for, 0 to turn caching off",
                },
                ParamSpec {
                    name: "rate-limit",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "how many requests are allowed in each interval",
                },
                ParamSpec {
                    name: "rate-limit-interval",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "the length of that interval in seconds",
                },
                ParamSpec {
                    name: "collect-logs",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "true to keep logs for requests through it, false to stop",
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },

        // ── §13.6, the Secrets Store ────────────────────────────────────────
        //
        // The mirror image of §13.10's service token, and worth reading beside
        // it. There, a secret Cloudflare issued came back and had to be kept
        // here. Here, a secret the owner holds goes the other way — and comes
        // back never, because `secrets-store_value` is `writeOnly` and
        // `x-sensitive` in Cloudflare's own schema: *"this is 'write only' —
        // the API never returns this value"*. So §13.6's second sentence is
        // the whole design: **APEX stores only the provider reference, and
        // never fictitious plaintext.** Nothing below writes to this service's
        // own store.
        //
        // **Where the value comes from, and why not a parameter.** A parameter
        // reaches the audit trail: `CapabilityRecord::summary` renders every
        // one as `name=value` and that string is the `detail` of every refused
        // line. A `value` parameter would therefore write the secret into a
        // file an administrator greps, on the one path where the operation did
        // not even happen. So the value arrives as the request BODY — the way
        // `apex secret add` sends one, as bytes after the request line rather
        // than as a field — or out of a file in the project, read under
        // `project::read_file`'s rules. One or the other, never both, never
        // neither. There is no `value` parameter and no operation here declares
        // one, so the framework refuses it before this module is asked.
        OperationSpec {
            id: "cloudflare.secret.create",
            summary: "put a secret into one of this project's Secrets Store \
                      stores, without it passing through the agent",
            effect: Effect::Write,
            // `<store>/<name>` — the store the project bound, and what to call
            // the secret in it.
            resource: WITHIN,
            params: &[SCOPES, COMMENT_ON_SECRET, VALUE_FILE],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.secret.rotate",
            summary: "replace the value of a secret in one of this project's \
                      Secrets Store stores",
            effect: Effect::Write,
            resource: WITHIN,
            params: &[COMMENT_ON_SECRET, VALUE_FILE],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.secret.bind",
            // **Binding is by reference, which is the point.** What goes onto
            // the worker is a store id and a secret name; the value stays in
            // the Secrets Store and is never read by anything here. That is
            // §13.6's "references/metadata only" applied to the verb that
            // sounds most like it would need plaintext.
            summary: "give one of this project's workers a reference to a \
                      secret in one of its stores",
            effect: Effect::Write,
            resource: WITHIN,
            params: &[
                ParamSpec {
                    name: "worker",
                    syntax: Syntax::Name,
                    required: true,
                    summary: "which of this project's workers to bind it to",
                },
                ParamSpec {
                    name: "binding",
                    syntax: Syntax::Name,
                    required: true,
                    summary: "the name the worker's code will read it under",
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },

        // ── §13.10, Cloudflare One ──────────────────────────────────────────
        //
        // Four of §13.2's names and one addition, and the thing that makes this
        // block different from every other one above: two of these endpoints
        // answer with a CREDENTIAL. `POST /access/service_tokens` hands back a
        // `client_secret` the API will never show again, and
        // `GET /cfd_tunnel/{id}/token` hands back a connector token as its whole
        // result. §13.10 says where those go — "directly into protected
        // storage… the agent receives handles/capabilities, not plaintext
        // secrets" — and `Bound::creates` is how they get there.
        //
        // Three things are deliberately NOT declared here, each for a reason
        // that would otherwise have to be discovered at use time:
        //
        // * **creating a tunnel.** `POST /cfd_tunnel` exists, and a tunnel is
        //   addressed by an id that does not exist until it does — so unlike an
        //   R2 bucket, which §13.1's file can name into existence, a tunnel
        //   cannot resolve through the project's own binding until after it has
        //   been made. The owner creates it and writes the id down;
        // * **reading a tunnel's token.** It is a credential, and a credential
        //   this build cannot pin honestly: `cloudflared` presents it to the
        //   Cloudflare edge over QUIC, not to any HTTPS host the store could
        //   name. Storing it would mean writing a `host` the pin cannot mean.
        //   §12's `cloudflared auth -> APEX` is where a broker-owned connector
        //   would spend it, and that is where it belongs;
        // * **a tunnel's `tunnel_secret`.** `PATCH /cfd_tunnel/{id}` accepts
        //   one, which would have an agent handing the broker a credential —
        //   the exact inverse of what this service is for, and the same
        //   argument `hyperdrive.edit` makes about an origin password.
        OperationSpec {
            id: "cloudflare.access.read",
            summary: "read the configuration of one of this project's Access \
                      applications",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.access.edit",
            // **What it is, and — because "edit" could mean anything — what it
            // is not.** It cannot change an application's policies, its
            // session duration, its name, or who may reach it. That is not
            // timidity: `PUT /access/apps/{app_id}` REPLACES an application,
            // the schema for one is a `oneOf` over eleven kinds, and a build
            // that read an application it only partly understood and wrote it
            // back would eventually widen an authorisation policy by omission.
            // Revoking the sessions is the one Access mutation that is
            // bounded, has no body, and cannot grant anybody anything.
            summary: "revoke every session issued for one of this project's \
                      Access applications — it cannot change who may reach it",
            effect: Effect::Write,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.access.service-token.create",
            // **The addition, and §13.3 is where it comes from**: its
            // networking/security list names "service tokens" beside "Access",
            // as `worker.route.read` came from the same list naming "Worker
            // routes". A separate name and not a parameter of `access.edit`,
            // because issuing a credential is a different decision from
            // revoking a session and an owner should get to make it separately.
            summary: "issue an Access service token for a host in this \
                      project's zone, and keep the secret in this service",
            effect: Effect::Write,
            // The HOST the token is for, checked against the project's bound
            // zone — not the token's name. The host is what the stored
            // credential gets pinned to, so it is the thing the project has to
            // be allowed to name.
            resource: NAMED,
            params: &[
                ParamSpec {
                    name: "name",
                    syntax: Syntax::Name,
                    required: true,
                    summary: "what to call the token in the Cloudflare dashboard",
                },
                ParamSpec {
                    name: "duration",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "how long it lives: 8760h, 730h, 168h, 24h or forever",
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.tunnel.read",
            summary: "read the status and connections of one of this project's \
                      tunnels — never its connector token",
            effect: Effect::Read,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.tunnel.edit",
            summary: "rename one of this project's tunnels — it cannot change \
                      what the tunnel routes or the secret it runs on",
            effect: Effect::Write,
            resource: NAMED,
            params: &[ParamSpec {
                name: "name",
                syntax: Syntax::Name,
                required: true,
                summary: "what to call it",
            }],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.r2.bucket.create",
            summary: "create one of the R2 buckets this project declares, in \
                      the account it is bound to",
            effect: Effect::Write,
            // A NAME, and one the project already lists under `buckets`. An
            // agent granted this cannot create a bucket the owner never wrote
            // down — the grant says which verb, and §13.1's file says which
            // thing, including a thing that does not exist yet.
            resource: NAMED,
            params: &[
                ParamSpec {
                    name: "location",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "the region hint to create it in: apac, eeur, enam, oc, weur or wnam",
                },
                ParamSpec {
                    name: "storage-class",
                    syntax: Syntax::Name,
                    required: false,
                    summary: "Standard or InfrequentAccess",
                },
            ],
            aliases: &[],
            same_everywhere: false,
        },
        // ---------------------------------------------------------------
        // P1-012: the tools, not the API.
        //
        // These four are NOT §13.2 names, and the two that came before them —
        // `worker.route.read` and `access.service-token.create` — set the
        // precedent: §13.2 is a vocabulary for the API surface, and §13.4 asks
        // for a broker-owned `wrangler` child in as many words. The membership
        // test skips them by name, as it skips those two.
        //
        // Each is one subcommand with an argv this build writes. There is
        // deliberately no `cloudflare.wrangler.run`: a grant to pass one's own
        // argv to wrangler is a grant to do everything wrangler can do, which
        // is the thing this provider's whole vocabulary exists not to be.
        // ---------------------------------------------------------------
        OperationSpec {
            id: "cloudflare.wrangler.deploy",
            summary: "deploy one of this project's workers with the project's \
                      own wrangler, run by the broker",
            effect: Effect::Write,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.wrangler.versions-upload",
            summary: "upload a new version of one of this project's workers \
                      with the project's own wrangler, without putting it in \
                      front of traffic",
            effect: Effect::Write,
            resource: NAMED,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.terraform.plan",
            summary: "show what terraform would change in this project, \
                      changing nothing",
            effect: Effect::Read,
            resource: ResourceKind::None,
            params: &[],
            aliases: &[],
            // Reads the project's own `.tf` files and its own state. Two
            // projects, two different plans, one stored credential.
            same_everywhere: false,
        },
        OperationSpec {
            id: "cloudflare.terraform.apply",
            summary: "make the changes terraform plans for this project",
            effect: Effect::Write,
            resource: ResourceKind::None,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
    ],
};

/// What a request resolved to.
///
/// The output of the semantic half. Every variant is a thing the *project*
/// bound, so an operation on something it did not bind has no variant to
/// become and is refused before any call is built.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// No account bound yet: list what this credential can see, so the id can
    /// be put in the file. The only operation that works before §13.1's
    /// binding exists, and `apex cf status` is built on it.
    Accounts,
    Account(Account),
    Worker(Worker),
    Route { zone: Zone, worker: Worker },
    /// A bucket this project declares. It is a target before it exists —
    /// `r2.bucket.create` is what makes it — which is why the project file
    /// listing it is what decides the name and not the other way round.
    Bucket(Bucket),
    /// An object in one of this project's buckets, or the bucket's own listing
    /// when the caller named no key.
    Object { bucket: Bucket, key: Option<String> },
    /// A database, namespace, queue or Hyperdrive config this project bound,
    /// and — for KV — the key under it. One variant for the four, because what
    /// distinguishes them is the path they build and not what they are.
    Bound {
        table: &'static str,
        resource: Resource,
        key: Option<String>,
    },
    /// A name in this project's zone. Which *record* at that name is a
    /// question only the zone can answer, and answering it costs a credential
    /// — so it happens in `perform` and not here.
    Record(Record),
    /// A model this project binds, run through a gateway it also binds. Two
    /// bound things, because both of them cost money and neither is the
    /// agent's to choose — Cloudflare creates a gateway on first use if the id
    /// does not exist, so a caller that could name one could bill one.
    Gatewayed { gateway: Resource, model: Resource },
    /// A secret in one of this project's stores, and the worker a reference to
    /// it is being put on. Two bound things, because binding changes both.
    SecretBinding {
        store: Resource,
        secret: String,
        worker: Worker,
    },
    /// A host in this project's zone that an Access service token is being
    /// issued for.
    ///
    /// The token itself is account-scoped at Cloudflare and names no host at
    /// all. The host is here because it is what the credential this operation
    /// creates gets **pinned** to, and a pin is only worth having if the
    /// project had to be allowed to name the host — so it resolves through
    /// [`Binding::record`], the same label-boundary and `records` narrowing
    /// §13.9 uses. A project that narrowed itself to some names cannot pin a
    /// credential to a different one.
    ServiceToken { account: Account, host: String },
}

/// The operations §13.8 protects: the ones that change what a worker **serves**.
///
/// ## Why this list is three names and not "every write"
///
/// §13.7's flow is `upload version -> preview -> health check -> staged traffic
/// -> full deployment`, and §13.8's table sits on the last two steps of it. A
/// build that asked for approval on every `Effect::Write` would ask for it at
/// step one — before anything had been previewed, before anything had been
/// health-checked, at the point where the agent has the least to show the
/// person being asked. That is the worst possible moment to interrupt somebody,
/// and it is how approval prompts get approved without being read.
///
/// So an upload to a protected environment is unattended. It puts a version on
/// Cloudflare and changes nothing anybody is using; §13.7 separated the two
/// halves precisely so this could be true.
///
/// **`cloudflare.secret.bind` is deliberately not here**, and it is the one
/// that took a decision rather than a reading. It changes a live worker's
/// bindings, which sounds like it belongs — but its RESOURCE is a secret in a
/// store, and the worker arrives as an *option*. An approval is keyed on the
/// operation and the resource, so protecting it would mean the owner approving
/// `store/secret` and the agent choosing the worker. That is an approval for a
/// thing the owner did not read, which is worse than no approval. Binding a
/// secret is also not a deployment: the worker keeps serving the same code
/// until something in this list runs.
const PROTECTED_OPERATIONS: &[&str] = &[
    "cloudflare.worker.deploy",
    "cloudflare.worker.rollback",
    "cloudflare.wrangler.deploy",
];

/// §13.8 for one resolved request.
///
/// Two things have to be true for the owner to be asked: the operation is one
/// that changes what is served, and the worker it names is in an environment
/// the project has not declared unattended.
fn protection(op: &OperationSpec, target: &Target) -> Approval {
    if !PROTECTED_OPERATIONS.contains(&op.id) {
        return Approval::Standing;
    }
    // `Target::Worker` and nothing else. `Route` also carries a worker, but
    // reading which routes a zone sends where changes nothing, and the day an
    // operation writes a route is the day it is added to the list above with
    // its own sentence.
    let Target::Worker(worker) = target else {
        return Approval::Standing;
    };
    match worker.protection {
        Protection::Unattended => Approval::Standing,
        Protection::ApprovalRequired => Approval::Required(format!(
            "'{}' is this project's {} worker, and {} is not an environment an \
             agent may deploy to on its own. Approve this one deployment with \
             `apex secret approve cloudflare {} {}`, or let every deployment to \
             it through by adding `unattended = true` under \
             [cloudflare.{}] in apex.toml",
            worker.name,
            worker.environment,
            worker.environment,
            op.id,
            worker.name,
            worker.environment,
        )),
    }
}

impl From<BindingError> for ProviderError {
    fn from(e: BindingError) -> ProviderError {
        match e {
            // A name the project does not bind is a resource that does not
            // exist *here*, which is a different answer from "you may not" and
            // from "it broke".
            BindingError::NoWorker { .. }
            | BindingError::NoZone { .. }
            | BindingError::NoBucket { .. }
            | BindingError::NoRecord { .. }
            | BindingError::NoResource { .. } => ProviderError::NoSuchResource(e.to_string()),
            // Everything else is a file that is wrong rather than a name that
            // is not bound — a missing id, an id that is not one, a project
            // with no Cloudflare section at all.
            _ => ProviderError::Refused(e.to_string()),
        }
    }
}

/// The Cloudflare provider.
pub struct CloudflareProvider {
    api: Api,
    tools: Tools,
}

impl CloudflareProvider {
    /// The one the daemon serves.
    pub fn new() -> CloudflareProvider {
        CloudflareProvider {
            api: Api::cloudflare(),
            tools: Tools::system(),
        }
    }

    /// One pointed at a loopback server, for the tests that are the only place
    /// any of this has ever run.
    #[cfg(test)]
    pub fn at(port: u16) -> CloudflareProvider {
        CloudflareProvider {
            api: Api::loopback(port),
            tools: Tools::system(),
        }
    }

    /// The same, with `wrangler` and `terraform` taken from a directory a test
    /// wrote them into. Neither is installed on the machine this was built on.
    #[cfg(test)]
    pub fn with_tools(mut self, dir: &std::path::Path) -> CloudflareProvider {
        self.tools = Tools::in_dir(dir);
        self
    }

    /// §13.1, applied: what did this project bind that name to?
    ///
    /// Reads no credential and makes no call, because
    /// [`crate::provider::Provider::bind`] may not. That constraint is the
    /// reason the account and zone **ids** are in the project file at all: the
    /// name-to-id lookup is an authenticated request, so it is an operation of
    /// its own rather than a hidden step inside every other one.
    /// The account a minted token is created in, and the resource its single
    /// policy names.
    ///
    /// Reads the project file a third time — `bind` read it, `perform` reads
    /// it again, and this is between them. The module note explains why
    /// re-reading is the shape this provider is in: `Bound` carries an
    /// endpoint and a sentence and nothing a provider defines, so there is no
    /// way to hand a resolution forward. `perform` already refuses if the file
    /// moved under it, which is the check that matters; a mint that read a
    /// file the operation then refuses to act on has cost a round trip and
    /// nothing else.
    /// P1-012: run one brokered subcommand with the credential in its
    /// environment and nothing else of this daemon's in there.
    fn run_brokered(
        &self,
        req: &Bind<'_>,
        brokered: &'static tools::Brokered,
        target: &Target,
        value: &SecretValue,
    ) -> Result<Performed, ProviderError> {
        let token = value.as_str().filter(|t| api::valid_token(t)).ok_or_else(|| {
            ProviderError::Refused(api::TransportError::BadCredential.to_string())
        })?;
        let Some(program) = self.tools.path(brokered.program) else {
            // Absent, and said as absent. "`wrangler` is not installed" and
            // "you may not run wrangler" have different fixes, and this
            // provider does not report the first as the second.
            return Err(ProviderError::Failed(format!(
                "`{}` is not installed on this machine, so this operation \
                 could not be carried out. It is not a permission this \
                 credential lacks",
                brokered.program.binary()
            )));
        };

        let mut args: Vec<String> = brokered.args.iter().map(|a| (*a).to_string()).collect();
        let mut extra: Vec<(&'static str, String)> = Vec::new();
        match target {
            Target::Worker(worker) => {
                extra.push(("CLOUDFLARE_ACCOUNT_ID", worker.account.id.clone()));
                if brokered.environment {
                    // The one thing a caller contributes to this command line,
                    // and it did not come from the caller: `resolve` got it out
                    // of the project's own `apex.toml`, which is why a name the
                    // project did not bind never reaches here.
                    args.push("--env".to_string());
                    args.push(worker.environment.clone());
                }
            }
            Target::Account(account) => {
                extra.push(("CLOUDFLARE_ACCOUNT_ID", account.id.clone()));
            }
            _ => {}
        }

        let tool = crate::broker::Tool {
            program: &program.to_string_lossy(),
            args: &args,
            cwd: std::path::Path::new(req.project),
            credential: (tools::CREDENTIAL_VARIABLE, token),
            extra,
        };
        let out = crate::broker::run_tool(&tool, req.owner)
            .map_err(ProviderError::Failed)?;
        Ok(Performed {
            code: out.code,
            output: out.text,
            created: None,
        })
    }

    fn narrowing(
        &self,
        req: &Bind<'_>,
        zone_scoped: bool,
    ) -> Result<(String, Scope, Narrowing), Minted> {
        let binding = match Binding::read(
            std::path::Path::new(req.project),
            req.owner.uid,
            &req.owner.name,
        ) {
            Ok(binding) => binding,
            Err(e) => {
                return Err(Minted::CouldNotRun(format!(
                    "this project's cloudflare binding could not be read, so no \
                     short-lived token was asked for: {e}"
                )))
            }
        };
        // Without an account id there is no endpoint to create a token at.
        // `cloudflare.account.read` is the operation that exists to find the
        // id out, and it necessarily runs before there is one — so this is a
        // could-not-run and not a refusal.
        let account = match binding.account() {
            Ok(account) => account,
            Err(e) => {
                return Err(Minted::CouldNotRun(format!(
                    "this project does not bind a cloudflare account id, so \
                     there is no account to create a short-lived token in: {e}"
                )))
            }
        };
        let strength = binding.temporary_credentials;
        if !zone_scoped {
            return Ok((
                account.id.clone(),
                Scope::Account(account.id.clone()),
                strength,
            ));
        }
        let target = match self.resolve(req) {
            Ok(target) => target,
            Err(e) => return Err(Minted::CouldNotRun(e.to_string())),
        };
        let zone = match &target {
            Target::Record(record) => record.zone.id.clone(),
            Target::Route { zone, .. } => zone.id.clone(),
            // A zone-scoped row in `POLICY` whose operation does not resolve
            // to something with a zone is this build disagreeing with itself.
            // It is not a refusal and it is not an absence.
            _ => {
                return Err(Minted::CouldNotRun(format!(
                    "'{}' is recorded as needing a zone-scoped token but does \
                     not resolve to a zone, so no short-lived token was asked \
                     for",
                    req.operation.id
                )))
            }
        };
        Ok((account.id.clone(), Scope::Zone(zone), strength))
    }

    fn resolve(&self, req: &Bind<'_>) -> Result<Target, ProviderError> {
        let binding = Binding::read(
            std::path::Path::new(req.project),
            req.owner.uid,
            &req.owner.name,
        );

        // Terraform acts on the project's whole Cloudflare footprint, so what
        // it resolves to is the account the project bound — the same target
        // `cloudflare.account.read` gets, and for the same reason: the thing
        // that decides what happens is the project's own directory.
        if matches!(
            req.operation.id,
            "cloudflare.terraform.plan" | "cloudflare.terraform.apply"
        ) {
            return Ok(Target::Account(binding?.account()?));
        }

        if req.operation.id == "cloudflare.account.read" {
            // The one that has to work before the file exists, so that there is
            // a way to find out what to put in it.
            return Ok(match binding.and_then(|b| b.account()) {
                Ok(account) => Target::Account(account),
                Err(_) => Target::Accounts,
            });
        }

        let binding = binding?;

        // Which noun the operation acts on decides which resolver runs, and
        // getting that wrong is not a silent bug: a build that resolved every
        // resource as a worker would answer "this project does not bind a
        // worker called 'example-assets'" for a bucket — a true sentence about
        // the wrong question, and the reader would go and add an environment.
        match req.operation.id {
            "cloudflare.r2.bucket.create" => {
                return Ok(Target::Bucket(binding.bucket(req.resource)?));
            }
            "cloudflare.r2.object.read" | "cloudflare.r2.object.write" => {
                let (bucket, key) = split_first(req.resource);
                return Ok(Target::Object {
                    bucket: binding.bucket(bucket)?,
                    key,
                });
            }
            "cloudflare.ai-gateway.run" => {
                let (gateway, model) = split_first(req.resource);
                let Some(model) = model else {
                    return Err(ProviderError::NoSuchResource(format!(
                        "'{gateway}' names a gateway and not a model to run \
                         through it: {gateway}/<model>"
                    )));
                };
                return Ok(Target::Gatewayed {
                    gateway: binding.resource("gateways", gateway)?,
                    model: binding.resource("models", &model)?,
                });
            }
            "cloudflare.workers-ai.run" => {
                return Ok(Target::Bound {
                    table: "models",
                    resource: binding.resource("models", req.resource)?,
                    key: None,
                });
            }
            "cloudflare.ai-gateway.edit" => {
                return Ok(Target::Bound {
                    table: "gateways",
                    resource: binding.resource("gateways", req.resource)?,
                    key: None,
                });
            }
            id if id.starts_with("cloudflare.secret.") => {
                // `<store>/<name>`: the store is bound, the secret inside it is
                // named. A project creates secrets, so a file listing every one
                // it will ever make could not be written in advance — the STORE
                // is where the boundary goes.
                let (store, name) = split_first(req.resource);
                let store = binding.resource("secrets", store)?;
                let Some(secret) = name else {
                    return Err(ProviderError::NoSuchResource(format!(
                        "'{}' names a store and not a secret in it. All three \
                         secret operations need both: {}/<name>",
                        store.name, store.name
                    )));
                };
                if id == "cloudflare.secret.bind" {
                    let Some(named) = req.params.get("worker") else {
                        return Err(ProviderError::Refused(
                            "this operation needs a 'worker' option saying which \
                             of this project's workers to bind it to"
                                .to_string(),
                        ));
                    };
                    return Ok(Target::SecretBinding {
                        store,
                        secret,
                        // Bound too, and by the same file: a reference may only
                        // be put on a worker this project owns.
                        worker: binding.worker(named)?,
                    });
                }
                return Ok(Target::Bound {
                    table: "secrets",
                    resource: store,
                    key: Some(secret),
                });
            }
            id if id.starts_with("cloudflare.kv.") => {
                let (namespace, key) = split_first(req.resource);
                return Ok(Target::Bound {
                    table: "kv",
                    resource: binding.resource("kv", namespace)?,
                    key,
                });
            }
            "cloudflare.access.service-token.create" => {
                // The host, not the token's name: see `Target::ServiceToken`.
                let record = binding.record(req.resource)?;
                return Ok(Target::ServiceToken {
                    account: binding.account()?,
                    host: record.name,
                });
            }
            id if id.starts_with("cloudflare.d1.")
                || id.starts_with("cloudflare.queue.")
                || id.starts_with("cloudflare.hyperdrive.")
                || id.starts_with("cloudflare.access.")
                || id.starts_with("cloudflare.tunnel.") =>
            {
                let table = match id.split('.').nth(1) {
                    Some("d1") => "d1",
                    Some("queue") => "queues",
                    Some("access") => "access",
                    Some("tunnel") => "tunnels",
                    _ => "hyperdrive",
                };
                return Ok(Target::Bound {
                    table,
                    resource: binding.resource(table, req.resource)?,
                    key: None,
                });
            }
            id if id.starts_with("cloudflare.dns.") => {
                let mut record = binding.record(req.resource)?;
                record.kind = CloudflareProvider::record_type(req)?;
                return Ok(Target::Record(record));
            }
            _ => {}
        }

        let worker = binding.worker(req.resource)?;
        if req.operation.id == "cloudflare.worker.route.read" {
            let Some(name) = binding.zone.clone() else {
                return Err(BindingError::NoZone {
                    path: binding.path.clone(),
                    named: req.resource.to_string(),
                    bound: None,
                }
                .into());
            };
            let zone = binding.zone(&name)?;
            return Ok(Target::Route { zone, worker });
        }
        Ok(Target::Worker(worker))
    }

    /// The sentence the audit trail records and the reply carries.
    ///
    /// Two jobs, and the second one constrains the first. It is what §11's
    /// trail records, so it has to read as a sentence; and it is the only
    /// thing `perform` can compare against what `bind` was pinned on, so it
    /// has to name **every** value that decides where the request goes. The
    /// account id is in it for that reason and not for decoration: an
    /// `account_id` swapped between the two reads keeps the account's label
    /// and changes the URL, and a sentence that carried only the label would
    /// let that through.
    fn detail(operation: &OperationSpec, target: &Target, params: &operation::Params) -> String {
        let version = params.get("version").map(String::as_str).unwrap_or("");
        // P1-012's four say which tool and which subcommand, because that is
        // the thing a person reading the trail needs: "deploy" through the
        // REST API and "deploy" through wrangler are different operations with
        // different blast radii, and a sentence that did not distinguish them
        // would make the trail agree with itself and mean nothing.
        if let Some(brokered) = tools::brokered(operation.id) {
            let what = format!("{} {}", brokered.program.binary(), brokered.args.join(" "));
            return match target {
                Target::Worker(worker) => format!(
                    "run `{what}` for {} ({}) in account {} [{}]",
                    worker.name,
                    worker.environment,
                    worker.account.named(),
                    worker.account.id
                ),
                Target::Account(account) => format!(
                    "run `{what}` in this project, against account {} [{}]",
                    account.named(),
                    account.id
                ),
                _ => format!("run `{what}` in this project"),
            };
        }
        match target {
            Target::Accounts => "list the accounts this credential can see".to_string(),
            Target::Account(account) => {
                format!("read account {} [{}]", account.named(), account.id)
            }
            Target::Route { zone, worker } => format!(
                "read the routes zone {} [{}] sends to {} ({})",
                zone.name, zone.id, worker.name, worker.environment
            ),
            Target::Bucket(bucket) => format!(
                "create the bucket {} in account {} [{}]",
                bucket.name,
                bucket.account.named(),
                bucket.account.id
            ),
            Target::Object { bucket, key } => {
                let where_ = format!(
                    "bucket {} in account {} [{}]",
                    bucket.name,
                    bucket.account.named(),
                    bucket.account.id
                );
                match (operation.id, key) {
                    ("cloudflare.r2.object.write", Some(key)) => {
                        // The file is in the sentence because it decides what
                        // is uploaded, and an owner reading the trail afterwards
                        // wants to know what went into the bucket.
                        let file = params.get("file").map(String::as_str).unwrap_or("");
                        format!("write {file} to object {key} in {where_}")
                    }
                    (_, Some(key)) => format!("read object {key} in {where_}"),
                    (_, None) => format!("list the objects in {where_}"),
                }
            }
            Target::Bound {
                table,
                resource,
                key,
            } => {
                let _ = table;
                let where_ = format!(
                    "{} [{}] in account {} [{}]",
                    resource.name,
                    resource.id,
                    resource.account.named(),
                    resource.account.id
                );
                match operation.id {
                    "cloudflare.d1.read" => format!("read database {where_}"),
                    "cloudflare.d1.query" => format!(
                        "run one statement against database {where_}: {}",
                        params.get("sql").map(String::as_str).unwrap_or("")
                    ),
                    "cloudflare.d1.migrate" => format!(
                        "apply {} to database {where_}",
                        params.get("file").map(String::as_str).unwrap_or("")
                    ),
                    "cloudflare.kv.read" => format!(
                        "read key {} in namespace {where_}",
                        key.as_deref().unwrap_or("")
                    ),
                    "cloudflare.kv.write" => format!(
                        "write key {} in namespace {where_}",
                        key.as_deref().unwrap_or("")
                    ),
                    "cloudflare.queue.publish" => format!("put a message on queue {where_}"),
                    "cloudflare.queue.manage" => {
                        format!("change the delivery settings of queue {where_}")
                    }
                    "cloudflare.hyperdrive.read" => {
                        format!("read hyperdrive configuration {where_}")
                    }
                    "cloudflare.hyperdrive.edit" => {
                        format!("change the name or caching of hyperdrive configuration {where_}")
                    }
                    "cloudflare.workers-ai.run" => format!("run the model {where_}"),
                    "cloudflare.ai-gateway.edit" => {
                        format!("change the caching and limits of ai gateway {where_}")
                    }
                    "cloudflare.secret.create" => format!(
                        "put the secret {} into store {where_}",
                        key.as_deref().unwrap_or("")
                    ),
                    "cloudflare.secret.rotate" => format!(
                        "replace the value of secret {} in store {where_}",
                        key.as_deref().unwrap_or("")
                    ),
                    "cloudflare.access.read" => {
                        format!("read the configuration of access application {where_}")
                    }
                    "cloudflare.access.edit" => format!(
                        "revoke every session issued for access application {where_}"
                    ),
                    "cloudflare.tunnel.read" => format!("read tunnel {where_}"),
                    "cloudflare.tunnel.edit" => format!(
                        "rename tunnel {where_} to {}",
                        params.get("name").map(String::as_str).unwrap_or("")
                    ),
                    other => format!("{other} on {where_}"),
                }
            }
            Target::Gatewayed { gateway, model } => format!(
                "run the model {} [{}] through ai gateway {} [{}] in account {} [{}]",
                model.name,
                model.id,
                gateway.name,
                gateway.id,
                gateway.account.named(),
                gateway.account.id
            ),
            Target::SecretBinding {
                store,
                secret,
                worker,
            } => format!(
                "give {} ({}) a reference to the secret {secret} in store {} [{}], \
                 as {}",
                worker.name,
                worker.environment,
                store.name,
                store.id,
                params.get("binding").map(String::as_str).unwrap_or("")
            ),
            Target::ServiceToken { account, host } => format!(
                // The sentence `perform` compares against what `bind` was
                // pinned on, so it names the account AND the host: those are
                // the two values that decide where the request goes and where
                // the credential it creates would be kept.
                "issue the access service token '{}' in account {} [{}], for {host}, \
                 and keep its secret as '{}'",
                params.get("name").map(String::as_str).unwrap_or(""),
                account.named(),
                account.id,
                token_service(host)
            ),
            Target::Record(record) => {
                let kind = record.kind.as_deref().unwrap_or("any");
                let where_ = format!("{} in zone {} [{}]", record.name, record.zone.name, record.zone.id);
                match operation.id {
                    "cloudflare.dns.read" => format!("read the {kind} records at {where_}"),
                    "cloudflare.dns.create" => format!(
                        "add a {kind} record at {where_} pointing at {}",
                        params.get("content").map(String::as_str).unwrap_or("")
                    ),
                    "cloudflare.dns.update" => format!("change the {kind} record at {where_}"),
                    "cloudflare.dns.delete" => format!("remove the {kind} record at {where_}"),
                    other => format!("{other} on the {kind} record at {where_}"),
                }
            }
            Target::Worker(worker) => {
                let where_ = format!(
                    "{} ({}) in account {} [{}]",
                    worker.name,
                    worker.environment,
                    worker.account.named(),
                    worker.account.id
                );
                match operation.id {
                    "cloudflare.worker.read" => format!("read the settings of {where_}"),
                    "cloudflare.worker.upload-version" => {
                        format!("upload a new version of {where_}")
                    }
                    // The share is in the sentence, so a staged rollout and a
                    // full deployment do not read the same in the trail — and
                    // so `perform`'s re-resolution check refuses if the two
                    // disagree about which of them was authorised.
                    "cloudflare.worker.deploy" => match params.get("percentage") {
                        Some(share) => format!(
                            "deploy version {version} of {where_} to {share}% of \
                             its traffic"
                        ),
                        None => format!("deploy version {version} of {where_}"),
                    },
                    "cloudflare.worker.rollback" => {
                        format!("roll {where_} back to version {version}")
                    }
                    "cloudflare.worker.tail" => format!("start a log session for {where_}"),
                    other => format!("{other} on {where_}"),
                }
            }
        }
    }

    /// Turn a resolved target into the call to make.
    ///
    /// Split from [`CloudflareProvider::resolve`] so that `bind` does not read
    /// a Worker bundle off the disk to answer a question about a hostname.
    fn build(&self, req: &Bind<'_>, target: &Target) -> Result<Call, ProviderError> {
        let get = |path: String| Call {
            method: "GET",
            path,
            body: Body::None,
            headers: Vec::new(),
        };
        Ok(match (req.operation.id, target) {
            ("cloudflare.account.read", Target::Accounts) => get("/accounts".to_string()),
            ("cloudflare.account.read", Target::Account(account)) => {
                get(format!("/accounts/{}", account.id))
            }
            ("cloudflare.worker.route.read", Target::Route { zone, .. }) => {
                get(format!("/zones/{}/workers/routes", zone.id))
            }
            ("cloudflare.worker.read", Target::Worker(worker)) => {
                get(format!("{}/settings", script(worker)))
            }
            ("cloudflare.worker.tail", Target::Worker(worker)) => Call {
                method: "POST",
                path: format!("{}/tails", script(worker)),
                body: Body::None,
                headers: Vec::new(),
            },
            ("cloudflare.worker.upload-version", Target::Worker(worker)) => Call {
                method: "POST",
                path: format!("{}/versions", script(worker)),
                body: self.version_body(req)?,
                headers: Vec::new(),
            },
            (id @ ("cloudflare.worker.deploy" | "cloudflare.worker.rollback"), Target::Worker(worker)) => {
                let rollback = id == "cloudflare.worker.rollback";
                Call {
                    method: "POST",
                    // `force` is what tells Cloudflare this is a deliberate
                    // return to an older version rather than a deployment that
                    // lost a race with a secret change. The endpoint is the
                    // same one `deploy` uses; what separates the two is the
                    // grant, which is the whole point of §13.2 naming them
                    // separately.
                    path: format!(
                        "{}/deployments{}",
                        script(worker),
                        if rollback { "?force=true" } else { "" }
                    ),
                    body: self.deployment_body(req)?,
                    headers: Vec::new(),
                }
            }
            ("cloudflare.r2.object.read", Target::Object { bucket, key }) => {
                get(objects(bucket, key.as_deref()))
            }
            ("cloudflare.r2.object.write", Target::Object { bucket, key }) => {
                let Some(key) = key else {
                    // `NoSuchResource` and not `Refused`: `example-assets` does
                    // not RESOLVE to an object, which is a different answer
                    // from "you may not write objects". The framework turns the
                    // two into `BadRequest` and `PermissionDenied`, and a
                    // caller sent to the grant table by a typo is the same
                    // defect this Cloudflare block has now produced three
                    // times.
                    return Err(ProviderError::NoSuchResource(format!(
                        "'{}' names a bucket and not an object. Writing needs a \
                         key as well: {}/<key>",
                        bucket.name, bucket.name
                    )));
                };
                Call {
                    method: "PUT",
                    path: objects(bucket, Some(key)),
                    body: self.object_body(req, key)?,
                    headers: Vec::new(),
                }
            }
            ("cloudflare.d1.read", Target::Bound { resource, .. }) => get(format!(
                "/accounts/{}/d1/database/{}",
                resource.account.id, resource.id
            )),
            (
                id @ ("cloudflare.d1.query" | "cloudflare.d1.migrate"),
                Target::Bound { resource, .. },
            ) => Call {
                method: "POST",
                // The same endpoint for both, which is not a shortcut: it is
                // what `wrangler d1 migrations apply` does. `/import` is a
                // three-step init/ingest/poll protocol around an upload to a
                // temporary URL, and that is not one brokered call. What
                // separates a migration from a query here is the grant, and
                // where the statements came from.
                path: format!(
                    "/accounts/{}/d1/database/{}/query",
                    resource.account.id, resource.id
                ),
                body: CloudflareProvider::sql_body(req, id == "cloudflare.d1.migrate")?,
                headers: Vec::new(),
            },
            ("cloudflare.kv.read", Target::Bound { resource, key, .. }) => {
                get(kv_value(resource, key.as_deref())?)
            }
            ("cloudflare.kv.write", Target::Bound { resource, key, .. }) => Call {
                method: "PUT",
                path: kv_value(resource, key.as_deref())?,
                body: self.kv_body(req)?,
                headers: Vec::new(),
            },
            ("cloudflare.queue.publish", Target::Bound { resource, .. }) => {
                let Some(message) = req.params.get("message") else {
                    return Err(ProviderError::Refused(
                        "this operation needs a 'message' option".to_string(),
                    ));
                };
                let mut body = serde_json::Map::new();
                body.insert("body".into(), message.clone().into());
                // Not the caller's to choose. A content type is a
                // header-shaped string, and the one thing this build sends is
                // a line of text.
                body.insert("content_type".into(), "text".into());
                Call {
                    method: "POST",
                    path: format!(
                        "/accounts/{}/queues/{}/messages",
                        resource.account.id, resource.id
                    ),
                    body: Body::Json(serde_json::Value::Object(body).to_string()),
                    headers: Vec::new(),
                }
            }
            ("cloudflare.queue.manage", Target::Bound { resource, .. }) => Call {
                method: "PATCH",
                path: format!("/accounts/{}/queues/{}", resource.account.id, resource.id),
                body: queue_settings(req)?,
                headers: Vec::new(),
            },
            ("cloudflare.hyperdrive.read", Target::Bound { resource, .. }) => get(format!(
                "/accounts/{}/hyperdrive/configs/{}",
                resource.account.id, resource.id
            )),
            ("cloudflare.hyperdrive.edit", Target::Bound { resource, .. }) => Call {
                // PATCH and never PUT: Cloudflare's PUT replaces a Hyperdrive
                // configuration and "must include the name and complete origin
                // connection details" — which means the origin password. There
                // is no way for this build to send one and no reason it should
                // ever hold one.
                method: "PATCH",
                path: format!(
                    "/accounts/{}/hyperdrive/configs/{}",
                    resource.account.id, resource.id
                ),
                body: hyperdrive_settings(req)?,
                headers: Vec::new(),
            },
            ("cloudflare.dns.read", Target::Record(record)) => {
                // The one read that does not need a lookup: the filter IS the
                // question. A name with no type gives every record at it.
                let mut path = format!("{}?name.exact={}", record.path(), record.name);
                if let Some(kind) = &record.kind {
                    path.push_str(&format!("&type={kind}"));
                }
                get(path)
            }
            ("cloudflare.dns.create", Target::Record(record)) => Call {
                method: "POST",
                path: record.path(),
                body: CloudflareProvider::record_body(req, record, true)?,
                headers: Vec::new(),
            },
            (
                "cloudflare.workers-ai.run",
                Target::Bound {
                    table: "models",
                    resource,
                    ..
                },
            ) => Call::new("POST", run_path(resource), inference_body(req)?),
            ("cloudflare.ai-gateway.run", Target::Gatewayed { gateway, model }) => Call {
                method: "POST",
                path: run_path(model),
                body: inference_body(req)?,
                // The two headers §13.11 and §13.14 need, and nothing else. The
                // gateway id comes out of the project's own file; the metadata
                // is composed here and carries no path and no prompt.
                headers: vec![
                    ("cf-aig-gateway-id", gateway.id.clone()),
                    ("cf-aig-metadata", usage_metadata(req)),
                ],
            },
            (
                "cloudflare.secret.create",
                Target::Bound {
                    resource,
                    key: Some(name),
                    ..
                },
            ) => Call {
                method: "POST",
                path: secrets(resource),
                // An ARRAY of one. The documented endpoint takes a list, and
                // this build sends exactly one element: a bulk create would
                // mean several values on one request, and there is one body.
                body: Body::Json(
                    serde_json::Value::Array(vec![secret_object(req, name, true)?]).to_string(),
                ),
                headers: Vec::new(),
            },
            ("cloudflare.access.read", Target::Bound { resource, .. }) => get(format!(
                "/accounts/{}/access/apps/{}",
                resource.account.id, resource.id
            )),
            ("cloudflare.access.edit", Target::Bound { resource, .. }) => Call {
                method: "POST",
                path: format!(
                    "/accounts/{}/access/apps/{}/revoke_tokens",
                    resource.account.id, resource.id
                ),
                // No body, and nothing a caller could put in one. That is what
                // makes this the Access mutation worth declaring: there is no
                // field to get wrong and no policy to widen.
                body: Body::None,
                headers: Vec::new(),
            },
            ("cloudflare.access.service-token.create", Target::ServiceToken { account, .. }) => {
                Call {
                    method: "POST",
                    path: format!("/accounts/{}/access/service_tokens", account.id),
                    body: service_token_body(req)?,
                    headers: Vec::new(),
                }
            }
            ("cloudflare.tunnel.read", Target::Bound { resource, .. }) => get(format!(
                "/accounts/{}/cfd_tunnel/{}",
                resource.account.id, resource.id
            )),
            ("cloudflare.tunnel.edit", Target::Bound { resource, .. }) => {
                let Some(name) = req.params.get("name") else {
                    return Err(ProviderError::Refused(
                        "this operation needs a 'name' option saying what to \
                         call it"
                            .to_string(),
                    ));
                };
                let mut body = serde_json::Map::new();
                body.insert("name".into(), name.clone().into());
                Call {
                    // PATCH and never the configurations PUT: `tunnel_secret`
                    // is the other field this endpoint accepts, and it is not
                    // declared, so the framework refuses it before this module
                    // is asked.
                    method: "PATCH",
                    path: format!(
                        "/accounts/{}/cfd_tunnel/{}",
                        resource.account.id, resource.id
                    ),
                    body: Body::Json(serde_json::Value::Object(body).to_string()),
                    headers: Vec::new(),
                }
            }
            ("cloudflare.r2.bucket.create", Target::Bucket(bucket)) => Call {
                method: "POST",
                path: format!("/accounts/{}/r2/buckets", bucket.account.id),
                body: bucket_body(req, bucket)?,
                headers: Vec::new(),
            },
            (id, _) => {
                return Err(ProviderError::Failed(format!(
                    "the cloudflare provider declares '{id}' and does not implement it"
                )))
            }
        })
    }

    /// The record type the caller named, checked in the order that gives the most
    /// useful refusal.
    ///
    /// §13.9's reserved shapes are checked **first**, so that asking to change an
    /// `NS` record is answered with the class it belongs to rather than with a
    /// remark about the type list. `SOA` is not a type Cloudflare will accept at
    /// all, and it still gets the §13.9 answer, because the person asking is
    /// trying to rewrite a zone's authority and deserves to be told that.
    fn record_type(req: &Bind<'_>) -> Result<Option<String>, ProviderError> {
        let Some(given) = req.params.get("type") else {
            return Ok(None);
        };
        let kind = given.to_ascii_uppercase();
        if req.operation.effect.is_write() {
            if let Some(why) = dns::elevated(&kind) {
                return Err(ProviderError::Refused(format!(
                    "changing a {kind} record is not something an ordinary project \
                     grant may do: {why}. §13.9 puts registrar, nameserver and \
                     DNSSEC-root changes in an elevated capability class, and this \
                     build has no such class — so it refuses rather than doing it \
                     under an ordinary grant. Reading one is allowed"
                )));
            }
        }
        if !dns::TYPES.contains(&kind.as_str()) {
            return Err(ProviderError::Refused(format!(
                "'{}' is not a DNS record type",
                given.escape_debug()
            )));
        }
        Ok(Some(kind))
    }

    /// The JSON body of a record create or update.
    ///
    /// `creating` is what separates the two, and the difference is deliberate:
    /// a create sends the whole record, and an update sends only the fields it was
    /// asked to change. The endpoint behind an update is `PATCH` for that reason —
    /// `PUT` overwrites a record with what the request carries, so an update that
    /// set only `content` through `PUT` would quietly reset the TTL and the proxy
    /// flag to whatever the request happened to leave out.
    fn record_body(req: &Bind<'_>, record: &Record, creating: bool) -> Result<Body, ProviderError> {
        let mut body = serde_json::Map::new();
        if creating {
            let Some(kind) = &record.kind else {
                return Err(ProviderError::Refused(
                    "this operation needs a 'type' option saying what kind of \
                     record to add"
                        .to_string(),
                ));
            };
            let Some(content) = req.params.get("content") else {
                return Err(ProviderError::Refused(
                    "this operation needs a 'content' option saying what the record \
                     points at"
                        .to_string(),
                ));
            };
            body.insert("type".into(), kind.clone().into());
            body.insert("name".into(), record.name.clone().into());
            body.insert("content".into(), content.clone().into());
        } else if let Some(content) = req.params.get("content") {
            body.insert("content".into(), content.clone().into());
        }

        if let Some(ttl) = req.params.get("ttl") {
            // 1 is Cloudflare's "automatic". Anything else is seconds, and a value
            // outside the range it accepts is refused here rather than spent.
            let seconds: u32 = ttl.parse().map_err(|_| {
                ProviderError::Refused(format!("'{}' is not a number of seconds", ttl.escape_debug()))
            })?;
            if seconds != 1 && !(60..=86400).contains(&seconds) {
                return Err(ProviderError::Refused(format!(
                    "{seconds} is not a TTL Cloudflare accepts: 1 for automatic, or \
                     60 to 86400 seconds"
                )));
            }
            body.insert("ttl".into(), seconds.into());
        }
        if let Some(proxied) = req.params.get("proxied") {
            let flag = match proxied.as_str() {
                "true" => true,
                "false" => false,
                other => {
                    return Err(ProviderError::Refused(format!(
                        "'{}' is not true or false",
                        other.escape_debug()
                    )))
                }
            };
            body.insert("proxied".into(), flag.into());
        }
        if let Some(comment) = req.params.get("comment") {
            body.insert("comment".into(), comment.clone().into());
        }

        if !creating && body.is_empty() {
            return Err(ProviderError::Refused(
                "this operation changes nothing: give it a 'content', 'ttl', \
                 'proxied' or 'comment' option"
                    .to_string(),
            ));
        }
        Ok(Body::Json(serde_json::Value::Object(body).to_string()))
    }

    /// The `{"sql": …}` body both D1 write operations send.
    ///
    /// `migrating` is where the two differ, and the difference is the reason
    /// §13.2 names them separately: a query carries one line the caller wrote,
    /// and a migration carries a file the project holds. A file may have as
    /// many statements and as many newlines as it likes, because it never
    /// passes through [`Syntax::Text`] — serde escapes it into the body.
    fn sql_body(req: &Bind<'_>, migrating: bool) -> Result<Body, ProviderError> {
        let sql = if migrating {
            let Some(file) = req.params.get("file") else {
                return Err(ProviderError::Refused(
                    "this operation needs a 'file' option naming the .sql file \
                     to apply"
                        .to_string(),
                ));
            };
            if !file.ends_with(".sql") {
                return Err(ProviderError::Refused(format!(
                    "'{}' is not a .sql file. A migration is SQL this project \
                     wrote down, not whatever happens to be at that path",
                    file.escape_debug()
                )));
            }
            let bytes = project::read_file(
                std::path::Path::new(req.project),
                file,
                req.owner.uid,
                &req.owner.name,
                MAX_PAYLOAD,
            )
            .map_err(|e| ProviderError::NoSuchResource(e.to_string()))?;
            String::from_utf8(bytes).map_err(|_| {
                ProviderError::Refused(format!(
                    "'{}' is not text, so it is not SQL",
                    file.escape_debug()
                ))
            })?
        } else {
            let Some(sql) = req.params.get("sql") else {
                return Err(ProviderError::Refused(
                    "this operation needs a 'sql' option".to_string(),
                ));
            };
            sql.clone()
        };
        let mut body = serde_json::Map::new();
        body.insert("sql".into(), sql.into());
        Ok(Body::Json(serde_json::Value::Object(body).to_string()))
    }

    /// What goes into a KV key: a line the caller wrote, or a file the project
    /// holds — one or the other, never both and never neither.
    fn kv_body(&self, req: &Bind<'_>) -> Result<Body, ProviderError> {
        match (req.params.get("value"), req.params.get("file")) {
            (Some(_), Some(_)) => Err(ProviderError::Refused(
                "give this operation a 'value' or a 'file', not both: they are \
                 two answers to the same question, and this build will not pick"
                    .to_string(),
            )),
            (None, None) => Err(ProviderError::Refused(
                "this operation needs a 'value' option holding what to store, \
                 or a 'file' option naming a file in the project to store"
                    .to_string(),
            )),
            (Some(value), None) => Ok(Body::Raw {
                content_type: "text/plain; charset=utf-8",
                bytes: value.clone().into_bytes(),
            }),
            (None, Some(file)) => {
                let bytes = project::read_file(
                    std::path::Path::new(req.project),
                    file,
                    req.owner.uid,
                    &req.owner.name,
                    MAX_PAYLOAD,
                )
                .map_err(|e| ProviderError::NoSuchResource(e.to_string()))?;
                Ok(Body::Raw {
                    content_type: "application/octet-stream",
                    bytes,
                })
            }
        }
    }

    /// The bytes of a project file, as the body of an R2 upload.
    ///
    /// Same read as a Worker module's — [`project::read_file`]'s `O_NOFOLLOW`
    /// walk, owned by the caller, capped — because it is the same problem: a
    /// root daemon opening a path a caller chose. What is different is that
    /// nothing here looks at the contents. An R2 object is whatever the project
    /// put in it.
    fn object_body(&self, req: &Bind<'_>, key: &str) -> Result<Body, ProviderError> {
        let Some(file) = req.params.get("file") else {
            return Err(ProviderError::Refused(
                "this operation needs a 'file' option naming the file inside \
                 the project to upload"
                    .to_string(),
            ));
        };
        let bytes = project::read_file(
            std::path::Path::new(req.project),
            file,
            req.owner.uid,
            &req.owner.name,
            MAX_PAYLOAD,
        )
        .map_err(|e| ProviderError::NoSuchResource(e.to_string()))?;
        Ok(Body::Raw {
            content_type: media_type(key),
            bytes,
        })
    }

    /// The multipart body for an upload: the module, and the metadata naming
    /// it.
    fn version_body(&self, req: &Bind<'_>) -> Result<Body, ProviderError> {
        let Some(script) = req.params.get("script") else {
            // The framework checks `required`, so this is unreachable through
            // the pipeline. It is here because a provider that trusted the
            // framework's checks would stop being correct the day they move.
            return Err(ProviderError::Refused(
                "this operation needs a 'script' option naming the module to \
                 upload"
                    .to_string(),
            ));
        };
        let module = script.rsplit('/').next().unwrap_or(script).to_string();
        if !module.ends_with(".js") && !module.ends_with(".mjs") {
            return Err(ProviderError::Refused(format!(
                "'{}' is not a module Cloudflare will accept as a worker's main \
                 module; it has to be a .js or .mjs file",
                module.escape_debug()
            )));
        }

        let bytes = project::read_file(
            std::path::Path::new(req.project),
            script,
            req.owner.uid,
            &req.owner.name,
            MAX_PAYLOAD,
        )
        .map_err(|e| ProviderError::NoSuchResource(e.to_string()))?;

        let mut metadata = serde_json::Map::new();
        metadata.insert("main_module".into(), module.clone().into());
        if let Some(date) = req.params.get("compatibility-date") {
            metadata.insert("compatibility_date".into(), date.clone().into());
        }
        if let Some(message) = req.params.get("message") {
            let mut annotations = serde_json::Map::new();
            annotations.insert("workers/message".into(), message.clone().into());
            metadata.insert("annotations".into(), annotations.into());
        }

        let mut form = Multipart::new().map_err(|e| ProviderError::Failed(e.to_string()))?;
        form.part(
            "metadata",
            None,
            "application/json",
            serde_json::Value::Object(metadata).to_string().as_bytes(),
        );
        form.part(
            &module,
            Some(&module),
            "application/javascript+module",
            &bytes,
        );
        Ok(form.finish())
    }

    /// The JSON body for a deployment: one version, all of the traffic.
    ///
    /// A split between two versions is §13.7's staged rollout and belongs to
    /// P1-013, which needs a health check to decide whether to widen it.
    /// §13.7's staged rollout body: two versions, adding to a hundred.
    ///
    /// The remainder is computed rather than taken from the caller, because a
    /// caller who could name both halves could name a pair that does not add
    /// up — and cloudflare's refusal of that would arrive as a validation
    /// error about a body this build composed.
    fn split_body(
        &self,
        req: &Bind<'_>,
        version: &str,
        share: f64,
        keeping: &str,
    ) -> Result<Body, ProviderError> {
        let entry = |id: &str, percentage: f64| {
            let mut entry = serde_json::Map::new();
            entry.insert("version_id".into(), id.into());
            entry.insert("percentage".into(), serde_json::json!(percentage));
            serde_json::Value::Object(entry)
        };
        let mut body = serde_json::Map::new();
        body.insert("strategy".into(), "percentage".into());
        body.insert(
            "versions".into(),
            serde_json::json!([
                entry(version, share),
                entry(keeping, deploy::remainder(share))
            ]),
        );
        if let Some(message) = req.params.get("message") {
            let mut annotations = serde_json::Map::new();
            annotations.insert("workers/message".into(), message.clone().into());
            body.insert("annotations".into(), annotations.into());
        }
        Ok(Body::Json(serde_json::Value::Object(body).to_string()))
    }

    fn deployment_body(&self, req: &Bind<'_>) -> Result<Body, ProviderError> {
        let Some(version) = req.params.get("version") else {
            return Err(ProviderError::Refused(
                "this operation needs a 'version' option naming the version to \
                 put in front of traffic"
                    .to_string(),
            ));
        };
        let mut entry = serde_json::Map::new();
        entry.insert("version_id".into(), version.clone().into());
        entry.insert("percentage".into(), serde_json::json!(100));

        let mut body = serde_json::Map::new();
        body.insert("strategy".into(), "percentage".into());
        body.insert("versions".into(), serde_json::json!([entry]));
        if let Some(message) = req.params.get("message") {
            let mut annotations = serde_json::Map::new();
            annotations.insert("workers/message".into(), message.clone().into());
            body.insert("annotations".into(), annotations.into());
        }
        Ok(Body::Json(serde_json::Value::Object(body).to_string()))
    }
}

impl Default for CloudflareProvider {
    fn default() -> CloudflareProvider {
        CloudflareProvider::new()
    }
}

/// The path prefix for one worker.
fn script(worker: &Worker) -> String {
    format!(
        "/accounts/{}/workers/scripts/{}",
        worker.account.id, worker.name
    )
}

/// The first segment of a resource, and whatever is left.
///
/// `example-assets/db/today.sql` is a bucket and a key, and the key keeps its
/// slashes because R2 object keys have them. The framework has already held
/// the whole string to [`apex_secret_core::operation::valid_path`] — no
/// leading or trailing `/`, no `//`, no `..`, no `:` — so the split cannot
/// produce an empty bucket or a key that climbs.
fn split_first(resource: &str) -> (&str, Option<String>) {
    match resource.split_once('/') {
        Some((first, rest)) => (first, Some(rest.to_string())),
        None => (resource, None),
    }
}

/// The path for one bucket's objects, or for one object in it.
fn objects(bucket: &Bucket, key: Option<&str>) -> String {
    let base = format!(
        "/accounts/{}/r2/buckets/{}/objects",
        bucket.account.id, bucket.name
    );
    match key {
        Some(key) => format!("{base}/{key}"),
        None => base,
    }
}

/// What to call an object's bytes, from the name the caller gave it.
///
/// A `&'static str` out of a fixed table rather than anything the caller sends,
/// because this ends up in a header. The default is deliberately the useless
/// one: an unknown extension is bytes, not a guess.
fn media_type(key: &str) -> &'static str {
    let extension = key.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match extension {
        "json" | "map" => "application/json",
        "txt" | "log" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "xml" => "application/xml",
        "sql" => "application/sql",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/vnd.microsoft.icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// The path of one key in one namespace.
fn kv_value(namespace: &Resource, key: Option<&str>) -> Result<String, ProviderError> {
    let Some(key) = key else {
        // See the note on R2's: a resource that does not resolve is
        // `NoSuchResource`, so the caller is told about the name and not about
        // a permission.
        return Err(ProviderError::NoSuchResource(format!(
            "'{}' names a namespace and not a key. Both KV operations need one: \
             {}/<key>",
            namespace.name, namespace.name
        )));
    };
    Ok(format!(
        "/accounts/{}/storage/kv/namespaces/{}/values/{key}",
        namespace.account.id, namespace.id
    ))
}

/// A whole number of seconds inside the range Cloudflare documents for it.
///
/// Refused here rather than sent, for the reason the R2 location hint is: a
/// request that fails at the far side has already spent the credential, and the
/// reply an owner reads then describes an API error instead of a typo.
fn seconds(
    value: &str,
    name: &str,
    range: std::ops::RangeInclusive<u32>,
) -> Result<u32, ProviderError> {
    let parsed: u32 = value.parse().map_err(|_| {
        ProviderError::Refused(format!(
            "'{}' is not a number of seconds",
            value.escape_debug()
        ))
    })?;
    if !range.contains(&parsed) {
        return Err(ProviderError::Refused(format!(
            "{parsed} is outside the range cloudflare accepts for {name}: {} to {}",
            range.start(),
            range.end()
        )));
    }
    Ok(parsed)
}

/// `true` or `false`, and nothing else.
fn flag(value: &str) -> Result<bool, ProviderError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ProviderError::Refused(format!(
            "'{}' is not true or false",
            other.escape_debug()
        ))),
    }
}

/// The `{"settings": …}` body of a queue change.
fn queue_settings(req: &Bind<'_>) -> Result<Body, ProviderError> {
    let mut settings = serde_json::Map::new();
    if let Some(paused) = req.params.get("paused") {
        settings.insert("delivery_paused".into(), flag(paused)?.into());
    }
    if let Some(delay) = req.params.get("delivery-delay") {
        settings.insert(
            "delivery_delay".into(),
            seconds(delay, "a delivery delay", 0..=86_400)?.into(),
        );
    }
    if let Some(retention) = req.params.get("retention") {
        settings.insert(
            "message_retention_period".into(),
            seconds(retention, "a retention period", 60..=1_209_600)?.into(),
        );
    }
    if settings.is_empty() {
        return Err(ProviderError::Refused(
            "this operation changes nothing: give it a 'paused', \
             'delivery-delay' or 'retention' option"
                .to_string(),
        ));
    }
    let mut body = serde_json::Map::new();
    body.insert("settings".into(), settings.into());
    Ok(Body::Json(serde_json::Value::Object(body).to_string()))
}

/// The body of a Hyperdrive change: a name and caching, and nothing that could
/// be a credential.
fn hyperdrive_settings(req: &Bind<'_>) -> Result<Body, ProviderError> {
    let mut body = serde_json::Map::new();
    if let Some(name) = req.params.get("name") {
        body.insert("name".into(), name.clone().into());
    }
    let mut caching = serde_json::Map::new();
    if let Some(on) = req.params.get("caching") {
        let on = match on.as_str() {
            "on" => true,
            "off" => false,
            other => {
                return Err(ProviderError::Refused(format!(
                    "'{}' is not on or off",
                    other.escape_debug()
                )))
            }
        };
        // Cloudflare's field is `disabled`. Inverting it here, rather than
        // asking an owner to grant something spelled in negatives, is the
        // difference between a capability somebody reads correctly and one
        // they do not.
        caching.insert("disabled".into(), (!on).into());
    }
    if let Some(max_age) = req.params.get("max-age") {
        caching.insert(
            "max_age".into(),
            seconds(max_age, "a cache age", 0..=86_400)?.into(),
        );
    }
    if let Some(stale) = req.params.get("stale-while-revalidate") {
        caching.insert(
            "stale_while_revalidate".into(),
            seconds(stale, "a stale window", 0..=86_400)?.into(),
        );
    }
    if !caching.is_empty() {
        body.insert("caching".into(), caching.into());
    }
    if body.is_empty() {
        return Err(ProviderError::Refused(
            "this operation changes nothing: give it a 'name', 'caching', \
             'max-age' or 'stale-while-revalidate' option"
                .to_string(),
        ));
    }
    Ok(Body::Json(serde_json::Value::Object(body).to_string()))
}

/// The fields `PUT /ai-gateway/gateways/{id}` documents, so a read-modify-write
/// can put back everything it did not mean to change.
///
/// Copied from the schema's own property list rather than from what the GET
/// happened to return: a `PUT` REPLACES a gateway, and a field this build drops
/// because it did not recognise it is a setting somebody loses.
const GATEWAY_FIELDS: &[&str] = &[
    "authentication",
    "byok_only",
    "cache_invalidate_on_update",
    "cache_ttl",
    "collect_logs",
    "dlp",
    "guardrails",
    "log_classification",
    "log_management",
    "log_management_strategy",
    "logpush",
    "logpush_public_key",
    "rate_limiting_interval",
    "rate_limiting_limit",
    "rate_limiting_technique",
    "retry_backoff",
    "retry_delay",
    "retry_max_attempts",
    "spend_limits",
    "store_id",
    "workers_ai_billing_mode",
    "zdr",
];

/// The body of a gateway change: everything it already had, plus what was asked
/// for.
///
/// **A read, then a write, because the endpoint is a `PUT` that replaces.** Its
/// schema marks `rate_limiting_interval`, `rate_limiting_limit`,
/// `collect_logs`, `cache_ttl` and `cache_invalidate_on_update` REQUIRED, so a
/// request carrying only the field being changed is not a smaller edit — it is
/// a rejected one, and everything else the gateway had would go back to a
/// default anyway.
///
/// **It refuses outright when the gateway carries an exporter credential.** A
/// gateway's `otel` list and its `stripe` block each hold an `authorization`,
/// and this build has two bad options and no good one: put it back as the GET
/// returned it — which writes whatever the API chose to show, possibly a
/// redaction — or leave it out, which deletes it. Refusing is the third thing,
/// and it is the only one that cannot silently break somebody's telemetry.
fn gateway_settings(
    api: &Api,
    gateway: &Resource,
    req: &Bind<'_>,
    value: &SecretValue,
) -> Result<Body, ProviderError> {
    let reply = api::call(
        api,
        &Call::new("GET", gateway_path(gateway), Body::None),
        value,
        req.owner,
    )
    .map_err(|e| ProviderError::Failed(e.to_string()))?;
    if !reply.ok() {
        return Err(ProviderError::Failed(format!(
            "cloudflare answered HTTP {} when this read gateway {}, so what it \
             already holds is not known and nothing was changed",
            reply.status, gateway.name
        )));
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&reply.body) else {
        return Err(ProviderError::Failed(
            "the gateway reply was not the json envelope, so nothing was changed"
                .to_string(),
        ));
    };
    let Some(current) = body.get("result").and_then(|r| r.as_object()) else {
        return Err(ProviderError::Failed(
            "the gateway reply carried no configuration, so nothing was changed"
                .to_string(),
        ));
    };
    if carries_authorization(body.get("result").unwrap_or(&serde_json::Value::Null)) {
        return Err(ProviderError::Refused(format!(
            "gateway {} carries an exporter credential — an `authorization` on \
             its OTel or Stripe configuration. Changing a gateway is a PUT that \
             replaces it, so this build would have to write that credential back \
             without being able to read it honestly, or drop it and delete it. \
             It will not do either: change this gateway in the Cloudflare \
             dashboard. Nothing was changed",
            gateway.name
        )));
    }

    let mut settings = serde_json::Map::new();
    for field in GATEWAY_FIELDS {
        if let Some(existing) = current.get(*field) {
            if !existing.is_null() {
                settings.insert((*field).to_string(), existing.clone());
            }
        }
    }

    let mut changed = false;
    if let Some(ttl) = req.params.get("cache-ttl") {
        settings.insert(
            "cache_ttl".into(),
            seconds(ttl, "a cache ttl", 0..=2_678_400)?.into(),
        );
        changed = true;
    }
    if let Some(limit) = req.params.get("rate-limit") {
        settings.insert(
            "rate_limiting_limit".into(),
            seconds(limit, "a rate limit", 0..=1_000_000)?.into(),
        );
        changed = true;
    }
    if let Some(interval) = req.params.get("rate-limit-interval") {
        settings.insert(
            "rate_limiting_interval".into(),
            seconds(interval, "a rate limiting interval", 0..=86_400)?.into(),
        );
        changed = true;
    }
    if let Some(collect) = req.params.get("collect-logs") {
        settings.insert("collect_logs".into(), flag(collect)?.into());
        changed = true;
    }
    if !changed {
        return Err(ProviderError::Refused(
            "this operation changes nothing: give it a 'cache-ttl', \
             'rate-limit', 'rate-limit-interval' or 'collect-logs' option"
                .to_string(),
        ));
    }
    // The five the schema marks required. A gateway that answered without one
    // of them would otherwise produce a request the far side refuses, after
    // the credential had been spent twice.
    for (field, fallback) in [
        ("rate_limiting_interval", serde_json::json!(0)),
        ("rate_limiting_limit", serde_json::json!(0)),
        ("collect_logs", serde_json::json!(true)),
        ("cache_ttl", serde_json::json!(0)),
        ("cache_invalidate_on_update", serde_json::json!(false)),
    ] {
        settings.entry(field.to_string()).or_insert(fallback);
    }
    Ok(Body::Json(serde_json::Value::Object(settings).to_string()))
}

/// Where one gateway lives.
fn gateway_path(gateway: &Resource) -> String {
    format!(
        "/accounts/{}/ai-gateway/gateways/{}",
        gateway.account.id, gateway.id
    )
}

/// Whether anything anywhere in a value is an `authorization`.
///
/// Deliberately blunt — any depth, any container. This decides whether a
/// replace is safe to attempt, and a check that looked only where the
/// credential is today would stop being right the moment Cloudflare adds a
/// third exporter.
fn carries_authorization(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.iter().any(|(key, nested)| {
                (key == "authorization" && !nested.is_null()) || carries_authorization(nested)
            })
        }
        serde_json::Value::Array(items) => items.iter().any(carries_authorization),
        _ => false,
    }
}

/// Where a model is run.
fn run_path(model: &Resource) -> String {
    format!("/accounts/{}/ai/run/{}", model.account.id, model.id)
}

/// The JSON body of an inference request: one line, or a document from the
/// project.
///
/// The same body-or-file shape §13.6's secrets use, for the same reason — a
/// prompt is one line and a conversation is not — with one addition.
/// **`stream` is refused.** The transport here is a `curl` that hands back a
/// completed reply; a streamed response would arrive as server-sent-event
/// framing in `output`, which is not a stream, is not the JSON the caller
/// expects, and would look like a working feature.
fn inference_body(req: &Bind<'_>) -> Result<Body, ProviderError> {
    let document = match (req.params.get("prompt"), req.params.get("file")) {
        (Some(_), Some(_)) => {
            return Err(ProviderError::Refused(
                "give this operation a 'prompt' or a 'file', not both: they are \
                 two answers to the same question, and this build will not pick"
                    .to_string(),
            ))
        }
        (None, None) => {
            return Err(ProviderError::Refused(
                "this operation needs a 'prompt' option holding what to ask, or \
                 a 'file' option naming a .json document in the project to send"
                    .to_string(),
            ))
        }
        (Some(prompt), None) => {
            let mut body = serde_json::Map::new();
            body.insert("prompt".into(), prompt.clone().into());
            serde_json::Value::Object(body)
        }
        (None, Some(file)) => {
            if !file.ends_with(".json") {
                return Err(ProviderError::Refused(format!(
                    "'{}' is not a .json file. A model request is JSON this \
                     project wrote down, not whatever happens to be at that path",
                    file.escape_debug()
                )));
            }
            let bytes = project::read_file(
                std::path::Path::new(req.project),
                file,
                req.owner.uid,
                &req.owner.name,
                MAX_PAYLOAD,
            )
            .map_err(|e| ProviderError::NoSuchResource(e.to_string()))?;
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return Err(ProviderError::Refused(format!(
                    "'{}' is not JSON, so it is not a model request",
                    file.escape_debug()
                )));
            };
            if !value.is_object() {
                return Err(ProviderError::Refused(format!(
                    "'{}' is JSON but not an object, and a model request is an \
                     object",
                    file.escape_debug()
                )));
            }
            value
        }
    };
    if document.get("stream").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(ProviderError::Refused(
            "this build cannot ask for a streamed response. The broker runs a \
             child that hands back a finished reply, so `stream: true` would \
             deliver server-sent-event framing in place of the answer — which \
             would look like it worked. Ask for it without `stream`"
                .to_string(),
        ));
    }
    Ok(Body::Json(document.to_string()))
}

/// What `cf-aig-metadata` carries, so §13.14's usage can be attributed.
///
/// Three entries of the five AI Gateway allows, all strings, none beginning
/// `cf.` — that prefix is Cloudflare's own and it strips customer-supplied keys
/// that use it.
///
/// **The audit id is the correlation key and the project path is not sent.**
/// This machine's trail already maps an audit id to a project, an agent session
/// and an origin; putting the id in the far side's log is enough to join the
/// two, and putting an owner's directory layout in somebody else's logs would
/// be more than enough. The project's own directory NAME goes in, reduced to
/// characters a header can carry — it is the one word that makes a gateway log
/// readable without a lookup.
fn usage_metadata(req: &Bind<'_>) -> String {
    let project = std::path::Path::new(req.project)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|name| {
            name.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .take(48)
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".to_string());
    serde_json::json!({
        "apex_audit": req.audit_id,
        "apex_project": project,
        "apex_operation": req.operation.id,
    })
    .to_string()
}

/// The path a store's secrets live under.
fn secrets(store: &Resource) -> String {
    format!(
        "/accounts/{}/secrets_store/stores/{}/secrets",
        store.account.id, store.id
    )
}

/// Cloudflare's own closed set of services that may read a stored secret.
const SCOPE_NAMES: &[&str] = &[
    "workers",
    "ai_gateway",
    "dex",
    "access",
    "containers",
    "websearch",
];

/// The `{name, value, scopes, comment}` object a create or a rotate sends.
///
/// `creating` decides which fields are required: a create names the secret and
/// its scopes, and a rotate changes a value on a secret that already has both.
fn secret_object(
    req: &Bind<'_>,
    name: &str,
    creating: bool,
) -> Result<serde_json::Value, ProviderError> {
    let mut object = serde_json::Map::new();
    if creating {
        object.insert("name".into(), name.into());
        let Some(scopes) = req.params.get("scopes") else {
            return Err(ProviderError::Refused(format!(
                "this operation needs a 'scopes' option saying which services \
                 may read it: {}",
                SCOPE_NAMES.join(", ")
            )));
        };
        let mut listed = Vec::new();
        for scope in scopes.split(',').map(str::trim) {
            if !SCOPE_NAMES.contains(&scope) {
                return Err(ProviderError::Refused(format!(
                    "'{}' is not a Cloudflare secret scope. One of: {}",
                    scope.escape_debug(),
                    SCOPE_NAMES.join(", ")
                )));
            }
            if !listed.iter().any(|s| s == scope) {
                listed.push(scope.to_string());
            }
        }
        if listed.is_empty() {
            return Err(ProviderError::Refused(
                "this operation needs at least one scope".to_string(),
            ));
        }
        object.insert("scopes".into(), listed.into());
    }
    object.insert("value".into(), secret_value(req)?.into());
    if let Some(comment) = req.params.get("comment") {
        object.insert("comment".into(), comment.clone().into());
    }
    Ok(serde_json::Value::Object(object))
}

/// The value of a secret: the bytes the caller piped in, or a file in the
/// project. One or the other, never both and never neither.
///
/// **Never a parameter**, and the reason is in §13.6's block note: a parameter
/// is rendered into `CapabilityRecord::summary` and becomes the `detail` of
/// every refused audit line, so a `value` option would write the secret into a
/// file an administrator greps — on the path where the operation did not even
/// happen.
fn secret_value(req: &Bind<'_>) -> Result<String, ProviderError> {
    /// `secrets-store_value`'s own `maxLength`.
    const MAX: usize = 64 * 1024;

    let bytes = match (req.body.is_empty(), req.params.get("file")) {
        (false, Some(_)) => {
            return Err(ProviderError::Refused(
                "this operation was given a value on its input AND a 'file'. \
                 They are two answers to the same question, and this build will \
                 not pick"
                    .to_string(),
            ))
        }
        (true, None) => {
            return Err(ProviderError::Refused(
                "this operation needs the secret's value. Pipe it in, or name a \
                 'file' inside the project that holds it — it is deliberately \
                 not an option, because an option is written to the audit trail"
                    .to_string(),
            ))
        }
        (false, None) => req.body.to_vec(),
        (true, Some(file)) => project::read_file(
            std::path::Path::new(req.project),
            file,
            req.owner.uid,
            &req.owner.name,
            MAX as u64,
        )
        .map_err(|e| ProviderError::NoSuchResource(e.to_string()))?,
    };
    if bytes.len() > MAX {
        return Err(ProviderError::Refused(format!(
            "that value is {} bytes and Cloudflare's limit is {MAX}",
            bytes.len()
        )));
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Err(ProviderError::Refused(
            "that value is not text, and a Secrets Store secret is a JSON string"
                .to_string(),
        ));
    };
    // One trailing newline, and one only. `printf %s` and `echo` differ by
    // exactly this byte, and a secret that works from one and not the other is
    // an afternoon somebody does not get back. A value that is ONLY newlines is
    // left alone, so this cannot empty something the caller meant to send.
    let trimmed = text.strip_suffix('\n').unwrap_or(&text);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Err(ProviderError::Refused(
            "that value is empty, and an empty secret is not one".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Which secret in a store a NAME means.
///
/// The same five answers as [`dns::look_up`], for the same reason: a credential
/// that was refused is not a store that is empty, and a build that collapsed
/// the two would tell a caller "no such secret" and have it create a second one
/// beside the first.
///
/// It has one hazard DNS does not. The endpoint's filter is `search`, and
/// **`search` is a substring match** — asking for `API_KEY` matches `API_KEY`
/// and `API_KEY_OLD` both. So every result is checked against the exact name
/// before anything is counted, and a build that took the first result back
/// would rotate the wrong secret. That is not a hypothetical shape of mistake;
/// it is the ordinary one.
fn look_up_secret(
    api: &Api,
    store: &Resource,
    name: &str,
    value: &SecretValue,
    owner: &crate::broker::Owner,
) -> Lookup {
    let call = Call {
        method: "GET",
        // `per_page` at the documented maximum, so that a store with many
        // similarly-named secrets is one page rather than a silent truncation.
        path: format!("{}?search={name}&per_page=100", secrets(store)),
        body: Body::None,
        headers: Vec::new(),
    };
    let reply = match api::call(api, &call, value, owner) {
        Ok(reply) => reply,
        Err(e) => return Lookup::CouldNotRun(e.to_string()),
    };
    if reply.status == 0 {
        return Lookup::CouldNotRun("the api could not be reached".to_string());
    }
    if reply.status == 401 || reply.status == 403 {
        return Lookup::Denied(reply.status);
    }
    if !reply.ok() {
        return Lookup::CouldNotRun(format!(
            "cloudflare answered HTTP {} to the lookup",
            reply.status
        ));
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&reply.body) else {
        return Lookup::CouldNotRun("the reply was not the json envelope".to_string());
    };
    if body.get("success").and_then(|s| s.as_bool()) != Some(true) {
        return Lookup::CouldNotRun("the store did not answer successfully".to_string());
    }
    let Some(results) = body.get("result").and_then(|r| r.as_array()) else {
        return Lookup::CouldNotRun("the reply carried no list of secrets".to_string());
    };
    let matching: Vec<&serde_json::Value> = results
        .iter()
        .filter(|r| r.get("name").and_then(|n| n.as_str()) == Some(name))
        .collect();
    if matching.is_empty() {
        // A page that did not hold it is not a store that does not hold it.
        // `total_count` is how the far side says there is more, and saying
        // "absent" here would be the same defect in a different costume.
        let total = body
            .get("result_info")
            .and_then(|i| i.get("total_count"))
            .and_then(serde_json::Value::as_u64);
        if total.is_some_and(|total| total > results.len() as u64) {
            return Lookup::CouldNotRun(format!(
                "the store answered with {} of {} secrets and this build reads \
                 one page, so whether '{name}' is there is not something this \
                 measured",
                results.len(),
                total.unwrap_or_default()
            ));
        }
        return Lookup::Absent;
    }
    match matching.len() {
        1 => match matching[0].get("id").and_then(|i| i.as_str()) {
            Some(id) if binding::valid_store_id(id) => Lookup::Found(id.to_string()),
            _ => Lookup::CouldNotRun("the secret it answered with has no usable id".to_string()),
        },
        n => Lookup::Ambiguous(n),
    }
}

/// The multipart body that puts a secret reference onto a worker.
///
/// A read-modify-write, because the settings `PATCH` replaces the `bindings`
/// list wholesale: sending only the new one would take every other binding off
/// the worker. So the existing list is read, the reference is added to it — or
/// replaces one of the same name — and the whole list goes back.
///
/// The window between the read and the write is real and is not closed here. It
/// is the same window `dns.update` has, and the same one `access.edit` would
/// have if this build rewrote applications: two requests cannot be one.
fn settings_with_secret(
    api: &Api,
    worker: &Worker,
    store: &Resource,
    secret: &str,
    binding_name: &str,
    value: &SecretValue,
    owner: &crate::broker::Owner,
) -> Result<Body, ProviderError> {
    let reply = api::call(
        api,
        &Call {
            method: "GET",
            path: format!("{}/settings", script(worker)),
            body: Body::None,
            headers: Vec::new(),
        },
        value,
        owner,
    )
    .map_err(|e| ProviderError::Failed(e.to_string()))?;
    if !reply.ok() {
        return Err(ProviderError::Failed(format!(
            "cloudflare answered HTTP {} when this read {}'s settings, so its \
             bindings are not known and nothing was changed",
            reply.status, worker.name
        )));
    }
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&reply.body) else {
        return Err(ProviderError::Failed(
            "the settings reply was not the json envelope, so nothing was changed"
                .to_string(),
        ));
    };
    // **Every existing binding goes back as `inherit`, and not as it arrived.**
    //
    // A `secret_text` binding's `text` is `writeOnly` AND required in
    // Cloudflare's own schema: the GET returns the binding without its value,
    // and sending that object back would either be refused for a missing
    // required field or quietly write an empty secret. The same is true of
    // every other kind that carries write-only material. `inherit` is the shape
    // the API provides for exactly this — "keep the binding that is already
    // there" — and using it for ALL of them, rather than enumerating which
    // kinds are safe to echo, is the version that stays correct when Cloudflare
    // adds the thirty-seventh kind.
    let mut bindings: Vec<serde_json::Value> = body
        .get("result")
        .and_then(|r| r.get("bindings"))
        .and_then(|b| b.as_array())
        .map(|existing| {
            existing
                .iter()
                .filter_map(|b| b.get("name").and_then(|n| n.as_str()))
                .map(|name| serde_json::json!({"type": "inherit", "name": name}))
                .collect()
        })
        .unwrap_or_default();
    // Same name, same binding: a worker cannot read two things under one name,
    // so this replaces rather than adding a second.
    bindings.retain(|b| b.get("name").and_then(|n| n.as_str()) != Some(binding_name));
    bindings.push(serde_json::json!({
        "type": "secrets_store_secret",
        "name": binding_name,
        "store_id": store.id,
        "secret_name": secret,
    }));

    let settings = serde_json::json!({ "bindings": bindings });
    let mut form = Multipart::new().map_err(|e| ProviderError::Failed(e.to_string()))?;
    form.part(
        "settings",
        None,
        "application/json",
        settings.to_string().as_bytes(),
    );
    Ok(form.finish())
}

/// What a credential created by [`SPEC`]'s service-token operation is stored
/// under.
///
/// Derived from the host rather than from the token's name, and the difference
/// matters: the name is a label in somebody's dashboard, and the host is what
/// the store pins the credential to. One brokered token per host, which is also
/// why a second one is refused rather than silently replacing the first —
/// Cloudflare shows a `client_secret` once.
fn token_service(host: &str) -> String {
    format!("cf-access.{host}")
}

/// Whether a string is a duration Cloudflare's service-token endpoint accepts.
///
/// `forever`, or Go's duration syntax — `8760h`, `2h45m`, `300ms` — with the
/// units the schema lists. Checked here rather than sent, for the reason the R2
/// location hint is: a request that fails at the far side has already spent the
/// credential, and for THIS operation a failed request may also have created a
/// token whose secret nobody will ever see.
fn valid_duration(value: &str) -> bool {
    const UNITS: &[&str] = &["ns", "us", "\u{b5}s", "ms", "s", "m", "h"];
    if value == "forever" {
        return true;
    }
    let mut rest = value;
    let mut pairs = 0;
    while !rest.is_empty() {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return false;
        }
        rest = &rest[digits..];
        // Longest unit first, or `ms` would be read as `m` followed by an `s`
        // that is not a number.
        let Some(unit) = UNITS
            .iter()
            .filter(|u| rest.starts_with(**u))
            .max_by_key(|u| u.len())
        else {
            return false;
        };
        rest = &rest[unit.len()..];
        pairs += 1;
    }
    pairs > 0
}

/// The JSON body that issues an Access service token.
fn service_token_body(req: &Bind<'_>) -> Result<Body, ProviderError> {
    let Some(name) = req.params.get("name") else {
        return Err(ProviderError::Refused(
            "this operation needs a 'name' option saying what to call the token"
                .to_string(),
        ));
    };
    let mut body = serde_json::Map::new();
    body.insert("name".into(), name.clone().into());
    if let Some(duration) = req.params.get("duration") {
        if !valid_duration(duration) {
            return Err(ProviderError::Refused(format!(
                "'{}' is not a duration Cloudflare accepts: 'forever', or a run \
                 of number-and-unit like 8760h or 2h45m, with units ns, us, ms, \
                 s, m or h",
                duration.escape_debug()
            )));
        }
        body.insert("duration".into(), duration.clone().into());
    }
    Ok(Body::Json(serde_json::Value::Object(body).to_string()))
}

/// The `client_id` and `client_secret` out of a service-token reply.
///
/// Both or neither. A reply that carries one and not the other is not a token
/// this build can keep, and saying so is better than storing half a credential
/// under a name that will then look usable.
fn service_token_in(body: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let result = value.get("result")?;
    let id = result.get("client_id")?.as_str()?.to_string();
    let secret = result.get("client_secret")?.as_str()?.to_string();
    if id.is_empty() || secret.is_empty() {
        return None;
    }
    Some((id, secret))
}

/// The JSON body that creates a bucket.
///
/// Both options are closed sets in Cloudflare's own documentation, so a value
/// outside them is refused here rather than sent and refused there. The reason
/// is not politeness: a request that fails at the far side has already spent
/// the credential, and the reply an owner reads then describes an API error
/// instead of a typo.
fn bucket_body(req: &Bind<'_>, bucket: &Bucket) -> Result<Body, ProviderError> {
    const LOCATIONS: &[&str] = &["apac", "eeur", "enam", "oc", "weur", "wnam"];
    const CLASSES: &[&str] = &["Standard", "InfrequentAccess"];

    let mut body = serde_json::Map::new();
    body.insert("name".into(), bucket.name.clone().into());
    if let Some(location) = req.params.get("location") {
        if !LOCATIONS.contains(&location.as_str()) {
            return Err(ProviderError::Refused(format!(
                "'{}' is not an R2 location hint. One of: {}",
                location.escape_debug(),
                LOCATIONS.join(", ")
            )));
        }
        body.insert("locationHint".into(), location.clone().into());
    }
    if let Some(class) = req.params.get("storage-class") {
        if !CLASSES.contains(&class.as_str()) {
            return Err(ProviderError::Refused(format!(
                "'{}' is not an R2 storage class. One of: {}",
                class.escape_debug(),
                CLASSES.join(", ")
            )));
        }
        body.insert("storageClass".into(), class.clone().into());
    }
    Ok(Body::Json(serde_json::Value::Object(body).to_string()))
}

/// A tail session's reply, with the part of it that is a credential taken out.
///
/// `POST …/tails` answers with `{"result":{"id":…,"url":"wss://…","expires_at":…}}`
/// and that `url` carries its own authorisation: whoever holds it can read the
/// worker's live logs without a token. Handing it back would be handing the
/// agent a credential, which is the one thing this whole service exists not to
/// do — so the session is created, its id and expiry are returned, and the URL
/// is dropped here.
///
/// The consequence is stated rather than hidden: **this build cannot deliver
/// the log stream.** Doing so means the broker holding the WebSocket and
/// relaying it, which is a different shape of operation from every other one in
/// the vocabulary and is named as P1-004's remainder.
fn without_the_tail_url(body: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    let removed = value
        .get_mut("result")
        .and_then(|r| r.as_object_mut())
        .map(|result| result.remove("url"))
        .unwrap_or(None);
    let mut out = serde_json::to_string(&value).unwrap_or_else(|_| body.to_string());
    if removed.is_some() {
        out.push_str(
            "\napex: the session's WebSocket URL is authorisation in itself, so \
             it is not returned. This build starts a tail session; it does not \
             relay the stream.",
        );
    }
    out
}

/// A Hyperdrive configuration, with anything credential-shaped taken out.
///
/// Cloudflare's own documentation says `origin.password` and
/// `access_client_secret` are write-only and that the API never returns them.
/// This removes them anyway, and the reason is the whole shape of this service:
/// **a broker that hands a caller a secret because the far side volunteered one
/// has still handed the caller a secret.** The guarantee an owner is given here
/// is that an agent gets capabilities and not credentials, and that guarantee
/// cannot rest on a remark in somebody else's documentation staying true.
///
/// The same argument as `without_the_tail_url`, applied to a field this build
/// does not expect to see rather than one it does.
fn without_the_origin_secrets(body: &str) -> String {
    without(
        body,
        &["password", "access_client_secret"],
        "\napex: this configuration carried an origin credential, which is \
         not something a brokered operation hands back. It has been removed \
         from this reply.",
    )
}

/// An Access application, with anything credential-shaped taken out.
///
/// An application is not obviously a place a secret lives, and for a
/// self-hosted one it is not. A **SaaS** application is a different matter:
/// Cloudflare's own schema gives `access_oidc_saas_app` and the generic OAuth
/// configuration a `client_secret`, and this operation is a read an agent may
/// hold. Same argument as the Hyperdrive password, applied to a field that is
/// only sometimes there — which is exactly the case a build finds out about in
/// production if it waits to be shown one.
fn without_the_app_secrets(body: &str) -> String {
    without(
        body,
        &["client_secret", "access_client_secret"],
        "\napex: this application carried a client secret, which is not \
         something a brokered read hands back. It has been removed from this \
         reply.",
    )
}

/// A reply with some named fields taken out of it, wherever they are.
///
/// Descends into nested objects and arrays, because that is where the field
/// actually is — a Hyperdrive password lives under `result.origin`, and a SaaS
/// client secret under `result.saas_app`. A scrub that only looked at the top
/// level would report success having removed nothing.
fn without(body: &str, keys: &[&str], note: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    let mut removed = false;
    let mut stack = vec![&mut value];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                for key in keys {
                    if map.remove(*key).is_some() {
                        removed = true;
                    }
                }
                stack.extend(map.values_mut());
            }
            serde_json::Value::Array(items) => stack.extend(items.iter_mut()),
            _ => {}
        }
    }
    let mut out = serde_json::to_string(&value).unwrap_or_else(|_| body.to_string());
    if removed {
        out.push_str(note);
    }
    out
}

/// The routes in a zone that point at one worker.
///
/// `GET /zones/{id}/workers/routes` answers with every route in the zone, and
/// the caller named a worker. Returning the lot would answer a question nobody
/// asked and would put the rest of the zone's routing in front of an agent
/// scoped to one worker — and the audit line would say "the routes zone X
/// sends to Y" about a list that is not that.
fn only_this_worker_s_routes(body: &str, worker: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    let Some(routes) = value.get_mut("result").and_then(|r| r.as_array_mut()) else {
        return body.to_string();
    };
    routes.retain(|route| route.get("script").and_then(|s| s.as_str()) == Some(worker));
    serde_json::to_string(&value).unwrap_or_else(|_| body.to_string())
}

impl Provider for CloudflareProvider {
    fn spec(&self) -> &'static ProviderSpec {
        &SPEC
    }

    fn bind(&self, req: &Bind<'_>) -> Result<Bound, ProviderError> {
        let target = self.resolve(req)?;
        // §13.10: an operation that will create a credential says so HERE, so
        // the framework can refuse a name that is already taken before
        // Cloudflare issues a secret it will never show again.
        let creates = match &target {
            Target::ServiceToken { host, .. } => {
                let name = token_service(host);
                if !apex_secret_core::store::valid_service_name(&name) {
                    return Err(ProviderError::Refused(format!(
                        "the credential this would create would be stored as \
                         '{name}', and that is not a name this service can \
                         store under. A host of up to {} characters fits",
                        64 - "cf-access.".len()
                    )));
                }
                Some(name)
            }
            _ => None,
        };
        Ok(Bound {
            endpoint: Endpoint {
                scheme: self.api.scheme.clone(),
                host: self.api.host.clone(),
            },
            detail: CloudflareProvider::detail(req.operation, &target, req.params),
            creates,
            // §13.8. The environment is in the binding, and this is the one
            // place that knows it.
            approval: protection(req.operation, &target),
        })
    }

    fn perform(
        &self,
        req: &Bind<'_>,
        bound: &Bound,
        value: &SecretValue,
    ) -> Result<Performed, ProviderError> {
        let target = self.resolve(req)?;
        // The project file is the caller's, and `bind` read it a moment ago.
        // If it says something else now, the pin and the trail describe an
        // operation that is not the one about to happen.
        let detail = CloudflareProvider::detail(req.operation, &target, req.params);
        if detail != bound.detail {
            return Err(ProviderError::Refused(
                "this project's Cloudflare binding changed while the request \
                 was being checked, so the operation that was authorised is \
                 not the one that would run. Try again"
                    .to_string(),
            ));
        }
        // §13.9's update and delete name a record the way a person does, and
        // the API addresses one by id — so the id is discovered here, with the
        // credential, and never taken from the caller. See [`dns`] for why an
        // id parameter would make the `records` narrowing meaningless.
        // P1-012: the operation is a child process rather than a request, so
        // it leaves before a `Call` is ever built. The credential reaching it
        // is whatever the framework presented — which, when §13.4's exchange
        // worked, is a token that expires in minutes and is deleted the moment
        // this returns.
        if let Some(brokered) = tools::brokered(req.operation.id) {
            return self.run_brokered(req, brokered, &target, value);
        }

        let call = match (req.operation.id, &target) {
            // §13.7's staged rollout. Cloudflare's payload is the WHOLE split
            // every time, so "send 30% here" cannot be expressed without also
            // saying where the other 70% goes — and the only honest answer to
            // that is the version that has it now. Asking costs a credential,
            // so it happens here and not in `bind`. See [`deploy`] for why
            // three of the five answers are refusals.
            ("cloudflare.worker.deploy", Target::Worker(worker))
                if req.params.get("percentage").is_some() =>
            {
                let raw = req.params.get("percentage").expect("checked by the guard");
                let share = deploy::share(raw).map_err(ProviderError::Refused)?;
                let Some(version) = req.params.get("version") else {
                    return Err(ProviderError::Refused(
                        "this operation needs a 'version' option naming the \
                         version to put in front of traffic"
                            .to_string(),
                    ));
                };
                if share >= deploy::MAX_SHARE {
                    // Not a rollout, just a deployment. Fall through to the
                    // one-request path rather than making a lookup whose
                    // answer cannot change anything.
                    self.build(req, &target)?
                } else {
                    let keeping = match deploy::current(&self.api, worker, value, req.owner) {
                        deploy::Current::One(id) if id == *version => {
                            return Err(ProviderError::Refused(format!(
                                "version {version} already has all of {}'s \
                                 traffic, so there is nothing to send {share}% \
                                 of it to",
                                worker.name
                            )))
                        }
                        deploy::Current::One(id) => id,
                        deploy::Current::None => {
                            return Err(ProviderError::Refused(format!(
                                "{} has never been deployed, so there is no \
                                 traffic to keep on an older version and a \
                                 share of it cannot be assigned. Deploy this \
                                 version without a 'percentage' option first",
                                worker.name
                            )))
                        }
                        deploy::Current::Split(ids) => {
                            return Err(ProviderError::Refused(format!(
                                "{}'s traffic is already split between {}, so \
                                 there is no single version to give the \
                                 remaining {}% to. Finish or undo that rollout \
                                 first — this build will not decide which of \
                                 them to retire",
                                worker.name,
                                ids.join(" and "),
                                deploy::remainder(share)
                            )))
                        }
                        deploy::Current::Denied(status) => {
                            return Err(ProviderError::Refused(format!(
                                "cloudflare answered HTTP {status} when this \
                                 asked which version is serving {}'s traffic. \
                                 That is the credential being refused, which is \
                                 not the same as the worker having no \
                                 deployments — so nothing was changed",
                                worker.name
                            )))
                        }
                        deploy::Current::CouldNotRun(why) => {
                            return Err(ProviderError::Failed(format!(
                                "this could not find out which version is \
                                 serving {}'s traffic, so it did not change \
                                 the split: {why}",
                                worker.name
                            )))
                        }
                    };
                    Call {
                        method: "POST",
                        path: format!("{}/deployments", script(worker)),
                        body: self.split_body(req, version, share, &keeping)?,
                        headers: Vec::new(),
                    }
                }
            }
            (id @ ("cloudflare.dns.update" | "cloudflare.dns.delete"), Target::Record(record)) => {
                let Some(kind) = record.kind.clone() else {
                    return Err(ProviderError::Refused(
                        "this operation needs a 'type' option: a name can hold \
                         several records and only the type says which one"
                            .to_string(),
                    ));
                };
                // Build the body BEFORE spending anything, so an update that
                // changes nothing is refused without a request being made.
                let body = if id == "cloudflare.dns.update" {
                    CloudflareProvider::record_body(req, record, false)?
                } else {
                    Body::None
                };
                let id_of = match dns::look_up(&self.api, record, &kind, value, req.owner) {
                    Lookup::Found(id) => id,
                    // The four answers that are not one record, each said in
                    // its own words. None of them builds a second request, and
                    // none of them is reported as any of the others.
                    Lookup::Absent => {
                        return Err(ProviderError::NoSuchResource(format!(
                            "zone {} answered, and holds no {kind} record at {}. \
                             Nothing was changed",
                            record.zone.name, record.name
                        )))
                    }
                    Lookup::Denied(status) => {
                        return Err(ProviderError::Refused(format!(
                            "cloudflare answered HTTP {status} when this looked \
                             up the {kind} record at {}. That is the credential \
                             being refused, which is not the same as the record \
                             not being there — so nothing was changed, and this \
                             build will not treat it as an absence",
                            record.name
                        )))
                    }
                    Lookup::Ambiguous(n) => {
                        return Err(ProviderError::Refused(format!(
                            "{n} {kind} records answer to {}. This build will \
                             not guess which one you meant, so nothing was \
                             changed",
                            record.name
                        )))
                    }
                    Lookup::CouldNotRun(why) => {
                        return Err(ProviderError::Failed(format!(
                            "the {kind} record at {} could not be looked up: \
                             {why}. Nothing was changed, and this is not a \
                             report that the record is absent",
                            record.name
                        )))
                    }
                };
                Call {
                    method: if id == "cloudflare.dns.update" { "PATCH" } else { "DELETE" },
                    path: format!("{}/{id_of}", record.path()),
                    body,
                    headers: Vec::new(),
                }
            }
            ("cloudflare.secret.rotate", Target::Bound { resource, key, .. }) => {
                let Some(name) = key else {
                    return Err(ProviderError::Refused(
                        "this operation needs a secret as well as a store".to_string(),
                    ));
                };
                // Built BEFORE anything is spent, so a rotate with no value is
                // refused without a request being made.
                let body = Body::Json(secret_object(req, name, false)?.to_string());
                let id_of = match look_up_secret(&self.api, resource, name, value, req.owner) {
                    Lookup::Found(id) => id,
                    Lookup::Absent => {
                        return Err(ProviderError::NoSuchResource(format!(
                            "store {} answered, and holds no secret called \
                             '{name}'. Nothing was changed — `cloudflare.secret.create` \
                             is what makes one",
                            resource.name
                        )))
                    }
                    Lookup::Denied(status) => {
                        return Err(ProviderError::Refused(format!(
                            "cloudflare answered HTTP {status} when this looked \
                             up '{name}' in store {}. That is the credential \
                             being refused, which is not the same as the secret \
                             not being there — so nothing was changed",
                            resource.name
                        )))
                    }
                    Lookup::Ambiguous(n) => {
                        return Err(ProviderError::Failed(format!(
                            "{n} secrets in store {} answer to the exact name \
                             '{name}'. This build will not guess which one you \
                             meant, so nothing was changed",
                            resource.name
                        )))
                    }
                    Lookup::CouldNotRun(why) => {
                        return Err(ProviderError::Failed(format!(
                            "'{name}' could not be looked up in store {}: {why}. \
                             Nothing was changed, and this is not a report that \
                             the secret is absent",
                            resource.name
                        )))
                    }
                };
                Call {
                    method: "PATCH",
                    path: format!("{}/{id_of}", secrets(resource)),
                    body,
                    headers: Vec::new(),
                }
            }
            ("cloudflare.ai-gateway.edit", Target::Bound { resource, .. }) => Call::new(
                "PUT",
                gateway_path(resource),
                gateway_settings(&self.api, resource, req, value)?,
            ),
            (
                "cloudflare.secret.bind",
                Target::SecretBinding {
                    store,
                    secret,
                    worker,
                },
            ) => {
                let Some(binding_name) = req.params.get("binding") else {
                    return Err(ProviderError::Refused(
                        "this operation needs a 'binding' option saying what the \
                         worker's code will read it under"
                            .to_string(),
                    ));
                };
                Call {
                    method: "PATCH",
                    path: format!("{}/settings", script(worker)),
                    body: settings_with_secret(
                        &self.api,
                        worker,
                        store,
                        secret,
                        binding_name,
                        value,
                        req.owner,
                    )?,
                    headers: Vec::new(),
                }
            }
            _ => self.build(req, &target)?,
        };
        let reply = api::call(&self.api, &call, value, req.owner)
            .map_err(|e| ProviderError::Failed(e.to_string()))?;

        // Everything the far side or the child chose comes back as output, not
        // as an error: the framework scrubs a result and does not scrub a
        // refusal reason.
        let body = match req.operation.id {
            "cloudflare.worker.tail" if reply.ok() => without_the_tail_url(&reply.body),
            "cloudflare.access.read" if reply.ok() => without_the_app_secrets(&reply.body),
            // The one reply in this module that IS a credential. The secret is
            // taken out here and the framework scrubs it again on the way out;
            // two layers, because the provider is where the shape is known and
            // the framework is where forgetting is not an option.
            "cloudflare.access.service-token.create" if reply.ok() => {
                without(&reply.body, &["client_secret"], "")
            }
            "cloudflare.hyperdrive.read" | "cloudflare.hyperdrive.edit" if reply.ok() => {
                without_the_origin_secrets(&reply.body)
            }
            "cloudflare.worker.route.read" if reply.ok() => {
                let name = match &target {
                    Target::Route { worker, .. } => worker.name.as_str(),
                    _ => "",
                };
                only_this_worker_s_routes(&reply.body, name)
            }
            _ => reply.body.clone(),
        };
        let output = if reply.ok() {
            body
        } else if reply.status == 0 {
            format!("apex: the api could not be reached\n{body}")
        } else {
            format!("{}\n{body}", answered_http(reply.status))
        };
        // §13.10, the half that needs the value: the secret goes to the
        // framework, which stores it under the name `bind` declared. What the
        // caller gets is the `client_id` and a sentence naming the handle.
        let mut created = None;
        let mut output = output;
        if req.operation.id == "cloudflare.access.service-token.create" && reply.ok() {
            let Target::ServiceToken { host, .. } = &target else {
                return Err(ProviderError::Failed(
                    "this operation resolved to something that is not a service \
                     token"
                        .to_string(),
                ));
            };
            match service_token_in(&reply.body) {
                Some((client_id, secret)) => {
                    created = Some(crate::provider::Created {
                        // The host the project named, which is what this
                        // credential may be sent to and nowhere else. An Access
                        // service token is presented to the application it
                        // guards, never to the API that issued it — so pinning
                        // it to `api.cloudflare.com` would be a pin that says
                        // the wrong thing.
                        host: host.clone(),
                        // Always https. Cloudflare Access does not protect
                        // anything else, and the store would refuse it anyway
                        // for a host that is not loopback.
                        scheme: "https".to_string(),
                        username: Some(client_id.clone()),
                        value: SecretValue::new(secret.into_bytes()),
                    });
                    output.push_str(&format!(
                        "\napex: the client id is {client_id}. Its secret is not \
                         in this reply — Cloudflare shows one once, and this \
                         service kept it."
                    ));
                }
                // The token EXISTS and its secret could not be read. Loud,
                // because the only thing that can be done about it is done by
                // a person: this build cannot fetch that secret again and
                // neither can anybody else.
                None => {
                    return Err(ProviderError::Failed(format!(
                        "cloudflare created a service token for {host} and \
                         answered with something this build could not read a \
                         client id and secret out of. The token exists and its \
                         secret is gone — delete it in the Cloudflare dashboard \
                         and try again"
                    )))
                }
            }
        }

        Ok(Performed {
            code: i32::from(!reply.ok()),
            output,
            created,
        })
    }

    /// §13.4, and P1-005's second acceptance criterion.
    ///
    /// The stored token is exchanged for one Cloudflare issued for this
    /// operation alone, at the narrowest scope the REST API can express, and
    /// [`Provider::revoke`] ends it as soon as the operation returns. The four
    /// answers and what each of them costs are in [`temporary`].
    fn mint(
        &self,
        req: &Bind<'_>,
        _bound: &Bound,
        value: &SecretValue,
    ) -> Result<Minted, ProviderError> {
        let Some(policy) = temporary::policy_for(req.operation.id) else {
            return Ok(Minted::NoNarrowerForm(format!(
                "this build knows no permission group that carries '{}', so it \
                 cannot describe a token narrower than the stored one",
                req.operation.id
            )));
        };
        let (account, scope, strength) = match self.narrowing(req, policy.zone_scoped()) {
            Ok(narrowing) => narrowing,
            Err(answer) => return Ok(answer),
        };
        if strength == Narrowing::Off {
            return Ok(Minted::NoNarrowerForm(format!(
                "{} sets `temporary_credentials = \"off\"` under [cloudflare], \
                 so no short-lived token was asked for",
                req.project
            )));
        }
        let answer = temporary::mint(
            &self.api,
            req.operation.id,
            req.audit_id,
            &account,
            &scope,
            value,
            req.owner,
        );
        // §13.4 says *prefer*, so `prefer` carries on with the stored
        // credential and the trail says why. A project that wrote `require`
        // has said the opposite, and the whole point of saying it is that the
        // operation does not quietly run on the broad token when the narrow
        // one stops being available.
        if strength == Narrowing::Require && answer.value().is_none() {
            return Err(ProviderError::Refused(format!(
                "{} sets `temporary_credentials = \"require\"` under \
                 [cloudflare], and this operation could not be given a \
                 short-lived credential, so it was not carried out with the \
                 stored one: {}",
                req.project,
                answer.reason().unwrap_or("no reason given")
            )));
        }
        Ok(answer)
    }

    fn revoke(
        &self,
        req: &Bind<'_>,
        _bound: &Bound,
        stored: &SecretValue,
        lease: &Lease,
    ) -> Result<(), String> {
        // Whether the policy was zone-scoped does not change which account the
        // token was created in, and the revoke addresses it by account. So the
        // `false` here is not a guess: a zone-scoped token and an
        // account-scoped one are deleted at the same path.
        let (account, _, _) = self
            .narrowing(req, false)
            .map_err(|answer| answer.reason().unwrap_or("no account").to_string())?;
        temporary::revoke(&self.api, &account, lease, stored, req.owner)
    }

    /// A 401, and deliberately not a 403.
    ///
    /// Cloudflare answers **401 with code 10000** when it does not recognise
    /// the credential — which is what a token it issued moments ago looks like
    /// to an API that has not seen it yet — and **403** when it recognises the
    /// credential and will not let it do this. The first is worth asking again
    /// and the second never will be: retrying a 403 would add nine seconds to
    /// every genuine permission failure and change none of them.
    ///
    /// Read off the line [`answered_http`] wrote, which is the only reason
    /// that line is a function: a build where the sentence and the reader
    /// drifted apart would go back to reporting a propagation delay as the
    /// operation's answer, silently.
    fn credential_refused(&self, performed: &Performed) -> bool {
        performed.output.lines().next() == Some(answered_http(401).as_str())
    }
}

/// The line [`Provider::perform`] writes when Cloudflare answered a status
/// this build treats as a failure.
///
/// One function because two places need the same sentence and they are 400
/// lines apart: the one that composes it and
/// [`CloudflareProvider::credential_refused`], which decides from it whether
/// the far side refused the credential itself.
pub fn answered_http(status: u16) -> String {
    format!("apex: cloudflare answered HTTP {status}")
}

#[cfg(test)]
mod tests;
