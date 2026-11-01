package com.rimeos.remote.core.agent

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Privilege requests and grants, as a paired phone may see them.
 *
 * ## The fact that shapes every screen built on this file
 *
 * **A phone can never approve or deny a privilege request, and no setting
 * changes that.** `rime-agentd/src/privilege.rs:1176` refuses `decide` from
 * any non-local origin unconditionally:
 *
 * ```rust
 * if !decider.is_local() {
 *     return Response::error(ErrorKind::PermissionDenied, …);
 * }
 * ```
 *
 * The check sits *before* the pending check, so a phone cannot even deny.
 * `Request::Decide` carries no second factor, and `decide` never consults
 * `OriginPolicy` — the `remote_elevation_allowed` opt-in gates a different
 * path (system-access grant issuance), not this one. `rime-remoted` declares
 * the origin `claude-remote-control` (`proxy.rs:128`), which is not local, and
 * `origin::may_declare` forbids narrowing back towards local. Pinned by
 * `rime-agentd/tests/request_origin.rs:369`.
 *
 * And every verb in the vocabulary is a root operation: `request.rs:290` maps
 * all eight to `Capability::RootCapability`, deliberately, because the
 * vocabulary was chosen to be exactly `rime`'s root-only subcommands.
 *
 * So this file deliberately has **no `decide` builder**. It is recorded as a
 * thing that does not exist rather than left to be noticed, because the
 * obvious next move on seeing a list of pending approvals is to add a button,
 * and that button would be refused by the daemon every time it was pressed.
 * [DECIDE_IS_LOCAL_ONLY] is what a screen shows instead.
 *
 * ## What a phone CAN do here
 *
 * List requests, list per-project grants, list system-access grants, and
 * **revoke** either kind — `revoke` and `revoke_system_grant` are gated on the
 * caller not being a managed session, and carry no origin check at all
 * (`privilege.rs:1289`, `privilege.rs:865`). Revocation only ever removes
 * authority, which is why it is safe from a device that cannot grant any.
 */
@Serializable
data class PrivilegeRequest(
    val id: Int = 0,
    /**
     * The verb, **flattened**: `Verb` is `#[serde(tag = "verb")]` inside a
     * `#[serde(flatten)]` field, so this key sits beside `id` rather than
     * under a `verb` object.
     *
     * The wire spelling is snake_case — `pkg_upgrade` — while the same verb is
     * hyphenated in [Grants] keys and in [SystemGrant.capabilities]. Both
     * appear in one screen; see [verbLabel].
     */
    val verb: String = "",
    /**
     * The packages, for `install` and `remove` only.
     *
     * **Absent**, not null, for the six verbs that take no arguments — those
     * are enum variants with no fields. Every other optional on this record is
     * an explicit null, because nothing in `rime-agent-core` is
     * `skip_serializing_if`.
     */
    val packages: List<String> = emptyList(),
    /** Why the agent says it needs this. Agent-written free text, ≤400 chars. */
    val reason: String = "",
    /** The asking session, resolved by the daemon from peer credentials. */
    val session: Int? = null,
    val agent: String? = null,
    val project: String? = null,
    /** §7's origin, **kebab-case**: `local-terminal`, `claude-remote-control`, … */
    @SerialName("request_origin") val requestOrigin: String? = null,
    /** How the origin was arrived at: `observed`, `declared`, `inherited`. */
    @SerialName("origin_source") val originSource: String? = null,
    /** Which remote actor asked: a paired device id, never a device name. */
    val actor: String? = null,
    /** `pending`, `allow_once`, `allow_for_project`, `denied`. snake_case. */
    val decision: String = PENDING,
    @SerialName("created_ms") val createdMs: Long = 0,
    @SerialName("decided_ms") val decidedMs: Long? = null,
    @SerialName("executed_ms") val executedMs: Long? = null,
    @SerialName("exit_code") val exitCode: Int? = null,
    @SerialName("system_grant") val systemGrant: Int? = null,
) {
    val isPending: Boolean get() = decision == PENDING
    val isDenied: Boolean get() = decision == DENIED
    val isAllowed: Boolean get() = decision == ALLOW_ONCE || decision == ALLOW_FOR_PROJECT

    /** Whether the origin that filed it was a local one. Display only. */
    val fromRemote: Boolean get() = requestOrigin != null && requestOrigin !in LOCAL_ORIGINS

    /**
     * The verb as a person reads it, hyphenated to match grant keys.
     *
     * The wire sends `pkg_upgrade` and a grant for the same operation is keyed
     * `pkg-upgrade`. A screen showing both would otherwise appear to list two
     * different operations.
     */
    val verbLabel: String get() = verb.replace('_', '-')

    /** The operation in words, including its arguments. */
    val effect: String
        get() = when (verb) {
            "install" -> "install ${packages.joinToString(", ")} on this machine"
            "remove" -> "remove ${packages.joinToString(", ")} from this machine"
            "pkg_upgrade" -> "bring every installed package up to date"
            "pkg_rebuild" -> "rebuild the installed packages for the running system"
            "pkg_rollback" -> "go back to the previous set of installed packages"
            "pin" -> "pin the current deployment so an update cannot collect it"
            "rollback" -> "boot the previous deployment on the next restart"
            "update" -> "update the OS image and firmware"
            // A verb added to the daemon after this build. Named, not guessed:
            // inventing a sentence for an operation this app has never heard of
            // would be describing a root action it cannot describe.
            else -> "run the `$verbLabel` operation"
        }

    /**
     * Whether this request was already covered by a standing decision.
     *
     * Worth its own name because `allow_for_project` does **not** mean the
     * operation ran: `request.rs:410` says execution still goes through the
     * approving human's own privilege, so a granted request continues to wait
     * for `rime request approve` at the machine. A phone that drew "allowed"
     * as "done" would be telling the user work had happened that had not.
     */
    val awaitingExecution: Boolean get() = isAllowed && executedMs == null

    companion object {
        const val PENDING = "pending"
        const val ALLOW_ONCE = "allow_once"
        const val ALLOW_FOR_PROJECT = "allow_for_project"
        const val DENIED = "denied"

        /**
         * `RequestOrigin::is_local()`, as the kebab names it covers. The shell's
         * under both spellings: a desktop from before the rebrand sends
         * `apex-shell`, and this app talks to both generations. (rime-rename: keep)
         */
        val LOCAL_ORIGINS = setOf("local-terminal", "rime-shell", "apex-shell") // rime-rename: keep (read both origin spellings)

        /**
         * What a phone says where a desktop would put Allow and Deny.
         *
         * Quoting the daemon's own refusal rather than paraphrasing it, so the
         * sentence the user reads here is the sentence they would get if the
         * button existed and they pressed it.
         */
        const val DECIDE_IS_LOCAL_ONLY: String =
            "Approve this at the machine. §7 reserves root operations for a human " +
                "at the computer, whichever origin filed them, so Rime refuses a " +
                "decision from a paired device — there is no setting that allows it."
    }
}

/**
 * A system-access grant: a bounded window in which a session may ask for
 * root operations without prompting each time.
 *
 * `capabilities` uses the **hyphenated** verb names (`Verb::name()`), unlike
 * [PrivilegeRequest.verb]. An empty list is legal and means break-glass.
 */
@Serializable
data class SystemGrant(
    val id: Int = 0,
    /** `system_access` or `break_glass`. snake_case on the wire. */
    val kind: String = "",
    val session: Int = 0,
    val agent: String = "",
    val project: String? = null,
    val capabilities: List<String> = emptyList(),
    @SerialName("issued_ms") val issuedMs: Long = 0,
    @SerialName("expires_ms") val expiresMs: Long = 0,
    @SerialName("boot_id") val bootId: String = "",
    @SerialName("request_origin") val requestOrigin: String = "",
    /** The polkit action id, or how a key-approved grant was authenticated. */
    @SerialName("authenticated_by") val authenticatedBy: String = "",
    val closed: GrantClosure? = null,
) {
    val isBreakGlass: Boolean get() = kind == "break_glass"
    val isOpen: Boolean get() = closed == null

    companion object {
        const val SYSTEM_ACCESS = "system_access"
        const val BREAK_GLASS = "break_glass"
    }
}

/**
 * How a grant ended.
 *
 * `why` uses `revoked` / `expired` / `reboot` / `runtime_restart`. The DISPLAY
 * vocabulary the daemon sends beside the grant in [GrantStates] is a different
 * set of words for the same concept — `ended-at-reboot`,
 * `ended-with-the-runtime` — so the two must not share one type.
 */
@Serializable
data class GrantClosure(
    val ms: Long = 0,
    val why: String = "",
)

/**
 * The state word and sentence the daemon computed for each grant.
 *
 * Sent rather than derived, because a grant's state depends on the running
 * kernel's boot id and a client deriving it would have to read `/proc` — which
 * a phone cannot do at all, for a machine it is not on.
 *
 * On the wire this is `[["active","14m left"], …]`: a JSON array of
 * two-element arrays, **not** objects, because serde serializes a Rust tuple
 * that way.
 */
data class GrantState(val word: String, val sentence: String)

/** Per-project grants: project root -> [Verb::grant_key] strings. */
typealias Grants = Map<String, List<String>>
