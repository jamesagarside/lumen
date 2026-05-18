import {
  createMemo,
  createSignal,
  For,
  Index,
  Match,
  onMount,
  Show,
  Switch,
  type Component,
  type JSX,
} from "solid-js";
import {
  createSettingsStore,
  type IntegrationDiagnostic,
  type IntegrationId,
  type IntegrationStatus,
  type OidcGroupMapping,
  type UserRole,
} from "./settingsStore";
import {
  createUserAdminStore,
  type RoleInfo,
  type UserDetail,
} from "./userAdminStore";

interface AdminSettingsProps {
  onClose: () => void;
}

/// Stable id for every section the admin panel can show. Three kinds:
///   - `IntegrationId` for things `/admin/settings` knows about (incl. OIDC)
///   - `"users"` for the Users admin section (`/admin/users` API)
///   - `"roles"` for the read-only role/capability matrix
type SectionId = IntegrationId | "users" | "roles";

/// Admin overlay shell. Sidebar layout: left rail lists every
/// admin-managed thing (grouped by category), right pane shows the
/// selected entry's form. Adding a new section means: append to
/// `SIDEBAR_GROUPS`, widen `SectionId`, add a `<Match>` clause below.
const AdminSettings: Component<AdminSettingsProps> = (props) => {
  const store = createSettingsStore();
  const userAdmin = createUserAdminStore();
  const [selected, setSelected] = createSignal<SectionId>("users");

  onMount(() => {
    void store.refresh();
    void userAdmin.refresh();
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    window.addEventListener("keydown", handler);
    // Refresh integration status every 5s so diagnostics stay live
    // without the user having to re-save. 5s lines up nicely with the
    // slowest integration's poll interval (UniFi labels = 60s; UniFi
    // IPS = 30s) while keeping the panel feeling alive. The user
    // admin endpoint isn't polled — it only changes via the form.
    const poll = window.setInterval(() => void store.refresh(), 5000);
    return () => {
      window.removeEventListener("keydown", handler);
      window.clearInterval(poll);
    };
  });

  const byId = (id: IntegrationId): IntegrationStatus | undefined =>
    store.statuses()?.find((s) => s.id === id);

  return (
    <div
      class="fixed inset-0 z-50 bg-zinc-950/85 backdrop-blur-sm flex items-start justify-center overflow-y-auto py-12 px-4"
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div class="w-full max-w-4xl bg-zinc-950 border border-zinc-800 rounded-md shadow-2xl flex flex-col max-h-[85vh]">
        <header class="flex items-center justify-between px-6 py-4 border-b border-zinc-800 flex-shrink-0">
          <div>
            <h2 class="text-sm font-semibold tracking-tight text-zinc-100">Settings</h2>
            <p class="text-[10px] text-zinc-500 mt-0.5">
              Manage integrations, secrets, and platform configuration.
            </p>
          </div>
          <button
            type="button"
            onClick={props.onClose}
            class="text-zinc-500 hover:text-zinc-200 text-sm px-2 py-1"
            title="close (esc)"
          >
            ✕
          </button>
        </header>

        <Show when={store.error()}>
          {(err) => (
            <div class="mx-6 mt-4 px-3 py-2 text-[11px] text-rose-300 bg-rose-950/40 border border-rose-900/60 rounded">
              {err()}
            </div>
          )}
        </Show>

        <div class="flex-1 min-h-0 flex">
          <Sidebar
            statuses={store.statuses()}
            selected={selected()}
            onSelect={setSelected}
          />
          <main class="flex-1 min-h-0 overflow-y-auto px-6 py-5">
            <Switch>
              <Match when={selected() === "users"}>
                <UsersSection store={userAdmin} />
              </Match>
              <Match when={selected() === "roles"}>
                <RolesSection store={userAdmin} />
              </Match>
              <Match when={store.loading() && !store.statuses()}>
                <p class="text-[10px] text-zinc-600">loading…</p>
              </Match>
              <Match when={store.statuses() && selected() === "unifi_labels"}>
                <UnifiLabelsSection
                  status={byId("unifi_labels")}
                  onSave={store.saveUnifiLabels}
                  onClear={() => store.clear("unifi_labels")}
                />
              </Match>
              <Match when={store.statuses() && selected() === "unifi_ips"}>
                <UnifiIpsSection
                  status={byId("unifi_ips")}
                  onSave={store.saveUnifiIps}
                  onClear={() => store.clear("unifi_ips")}
                />
              </Match>
              <Match when={store.statuses() && selected() === "webhook"}>
                <WebhookSection
                  status={byId("webhook")}
                  onSave={store.saveWebhook}
                  onClear={() => store.clear("webhook")}
                />
              </Match>
              <Match when={store.statuses() && selected() === "oidc"}>
                <OidcSection
                  status={byId("oidc")}
                  onSave={store.saveOidc}
                  onClear={() => store.clear("oidc")}
                />
              </Match>
            </Switch>
          </main>
        </div>

        <footer class="px-6 py-3 border-t border-zinc-800 text-[10px] text-zinc-600 flex items-center justify-between flex-shrink-0">
          <span>Changes take effect immediately — pollers restart in place.</span>
          <Show when={store.loading() && store.statuses()}>
            <span class="text-amber-400">refreshing…</span>
          </Show>
        </footer>
      </div>
    </div>
  );
};

// ── Sidebar ──────────────────────────────────────────────────────────────────

interface SidebarEntry {
  id: SectionId;
  label: string;
  /** Free-form short tag rendered under the label, e.g. "UniFi · labels". */
  badge: string;
  /** True for non-integration entries (Users, Roles) — these skip the
   * integration-status dot lookup and use their own indicator. */
  isMeta?: boolean;
}

interface SidebarGroup {
  title: string;
  entries: SidebarEntry[];
  /** When present, rendered as a muted hint below the group title. */
  hint?: string;
}

/// The single place to register new sections. Adding one is two
/// changes: append an entry here and a `<Match>` clause in the shell
/// above.
const SIDEBAR_GROUPS: SidebarGroup[] = [
  {
    title: "Access",
    hint: "Who can sign in and what they can do.",
    entries: [
      { id: "users", label: "Users", badge: "Local accounts", isMeta: true },
      { id: "roles", label: "Roles & capabilities", badge: "Reference", isMeta: true },
    ],
  },
  {
    title: "Authentication",
    hint: "Single sign-on and identity provider integration.",
    entries: [
      { id: "oidc", label: "OIDC / SSO", badge: "Identity provider" },
    ],
  },
  {
    title: "Integrations",
    hint: "Read from / write to third-party systems.",
    entries: [
      { id: "unifi_labels", label: "UniFi auto-label", badge: "UniFi · labels" },
      { id: "unifi_ips", label: "UniFi IPS alerts", badge: "UniFi · detections" },
      { id: "webhook", label: "Detection webhook", badge: "Outbound · notifications" },
    ],
  },
];

const Sidebar: Component<{
  statuses: IntegrationStatus[] | null;
  selected: SectionId;
  onSelect: (id: SectionId) => void;
}> = (props) => {
  const findStatus = (id: SectionId) =>
    props.statuses?.find((s) => s.id === (id as IntegrationId));

  return (
    <nav class="w-56 border-r border-zinc-800/80 px-3 py-4 flex-shrink-0 overflow-y-auto">
      <For each={SIDEBAR_GROUPS}>
        {(group) => (
          <div class="mb-5 last:mb-0">
            <h3 class="text-[9px] uppercase tracking-wider text-zinc-500 px-2 mb-1.5">
              {group.title}
            </h3>
            <Show when={group.hint}>
              <p class="text-[9px] text-zinc-600 px-2 mb-2">{group.hint}</p>
            </Show>
            <ul class="space-y-px">
              <For each={group.entries}>
                {(entry) => (
                  <SidebarItem
                    entry={entry}
                    status={entry.isMeta ? undefined : findStatus(entry.id)}
                    active={props.selected === entry.id}
                    onSelect={() => props.onSelect(entry.id)}
                  />
                )}
              </For>
            </ul>
          </div>
        )}
      </For>
    </nav>
  );
};

const SidebarItem: Component<{
  entry: SidebarEntry;
  status: IntegrationStatus | undefined;
  active: boolean;
  onSelect: () => void;
}> = (props) => {
  const dotClass = () => {
    if (props.entry.isMeta) return "bg-zinc-600";
    const s = props.status;
    if (!s) return "bg-zinc-700";
    if (s.running) return "bg-emerald-400";
    if (s.plain_source || s.secret_configured) return "bg-amber-400";
    return "bg-zinc-700";
  };

  return (
    <li>
      <button
        type="button"
        onClick={props.onSelect}
        class="w-full text-left px-2 py-1.5 rounded text-[11px] transition-colors flex items-start gap-2"
        classList={{
          "bg-zinc-900 text-zinc-100": props.active,
          "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-900/60": !props.active,
        }}
      >
        <span class={`size-1.5 rounded-full mt-1.5 flex-shrink-0 ${dotClass()}`} />
        <span class="flex-1 min-w-0">
          <span class="block">{props.entry.label}</span>
          <span class="block text-[9px] text-zinc-600 mt-0.5">{props.entry.badge}</span>
        </span>
      </button>
    </li>
  );
};

// ── Section shell + status badges ────────────────────────────────────────────

interface SectionProps {
  title: string;
  description: string;
  status: IntegrationStatus | undefined;
  children: JSX.Element;
}

const Section: Component<SectionProps> = (props) => (
  <section class="space-y-4">
    <header class="flex items-start justify-between gap-4">
      <div>
        <h3 class="text-sm font-semibold text-zinc-100">{props.title}</h3>
        <p class="text-[10px] text-zinc-500 mt-1 max-w-prose">{props.description}</p>
      </div>
      <StatusBadges status={props.status} />
    </header>
    <div class="space-y-4">{props.children}</div>
    <DiagnosticsPanel status={props.status} />
  </section>
);

const StatusBadges: Component<{ status: IntegrationStatus | undefined }> = (props) => {
  const labels = () => {
    const s = props.status;
    if (!s) return [];
    const out: Array<{ text: string; tone: "ok" | "warn" | "info" | "muted" }> = [];
    if (s.running) {
      out.push({ text: "running", tone: "ok" });
    } else if (s.plain_source || s.secret_configured) {
      out.push({ text: "stopped", tone: "warn" });
    } else {
      out.push({ text: "not configured", tone: "muted" });
    }
    if (s.plain_source === "env" || s.secret_source === "env") {
      out.push({ text: "from .env", tone: "info" });
    }
    return out;
  };

  return (
    <div class="flex items-center gap-1.5 flex-shrink-0">
      <For each={labels()}>
        {(l) => (
          <span
            class="text-[9px] uppercase tracking-wider px-1.5 py-0.5 rounded border"
            classList={{
              "text-emerald-300 border-emerald-900 bg-emerald-950/40": l.tone === "ok",
              "text-amber-300 border-amber-900 bg-amber-950/40": l.tone === "warn",
              "text-sky-300 border-sky-900 bg-sky-950/40": l.tone === "info",
              "text-zinc-500 border-zinc-800": l.tone === "muted",
            }}
          >
            {l.text}
          </span>
        )}
      </For>
    </div>
  );
};

// ── Diagnostics panel ────────────────────────────────────────────────────────

const formatRelative = (epochMillis: number): string => {
  const seconds = Math.max(0, Math.round((Date.now() - epochMillis) / 1000));
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  return `${hours}h ago`;
};

const DiagnosticsPanel: Component<{ status: IntegrationStatus | undefined }> = (props) => {
  const diag = createMemo<IntegrationDiagnostic | undefined>(() => props.status?.diagnostics);
  const running = () => !!props.status?.running;
  const configured = () =>
    !!props.status?.plain_source || !!props.status?.secret_configured;

  return (
    <Show when={configured()}>
      <div class="mt-4 border-t border-zinc-800/80 pt-4">
        <h4 class="text-[9px] uppercase tracking-wider text-zinc-500 mb-2">Diagnostics</h4>
        <Switch
          fallback={
            <p class="text-[10px] text-zinc-600">
              {running()
                ? "Waiting for the first poll to complete…"
                : "Integration is stopped."}
            </p>
          }
        >
          <Match when={diag()?.last_outcome?.status === "ok"}>
            <DiagnosticOk diag={diag()!} />
          </Match>
          <Match when={diag()?.last_outcome?.status === "err"}>
            <DiagnosticErr diag={diag()!} />
          </Match>
        </Switch>
      </div>
    </Show>
  );
};

const DiagnosticOk: Component<{ diag: IntegrationDiagnostic }> = (props) => {
  const outcome = () => {
    const o = props.diag.last_outcome;
    return o?.status === "ok" ? o : null;
  };
  return (
    <div class="space-y-1.5 text-[10px] text-zinc-400">
      <p>
        <span class="text-emerald-400">●</span>{" "}
        <span class="text-zinc-200">Last poll</span>
        <Show when={props.diag.last_poll_at}>
          {(t) => <span class="text-zinc-500"> · {formatRelative(t())}</span>}
        </Show>
        <Show when={outcome()}>
          {(o) => (
            <span class="text-zinc-500">
              {" · "}
              {o().observed} observed
              {o().new > 0 && (
                <span class="text-amber-300"> · {o().new} new this poll</span>
              )}
            </span>
          )}
        </Show>
      </p>
      <Show when={props.diag.last_item_summary}>
        {(summary) => (
          <p class="text-zinc-500 truncate" title={summary()}>
            most recent: <span class="text-zinc-300">{summary()}</span>
          </p>
        )}
      </Show>
    </div>
  );
};

const DiagnosticErr: Component<{ diag: IntegrationDiagnostic }> = (props) => {
  const outcome = () => {
    const o = props.diag.last_outcome;
    return o?.status === "err" ? o : null;
  };
  return (
    <div class="space-y-1.5 text-[10px] text-zinc-400">
      <p>
        <span class="text-rose-400">●</span>{" "}
        <span class="text-zinc-200">Last poll failed</span>
        <Show when={props.diag.last_poll_at}>
          {(t) => <span class="text-zinc-500"> · {formatRelative(t())}</span>}
        </Show>
      </p>
      <Show when={outcome()}>
        {(o) => (
          <p class="text-rose-300 font-mono text-[10px] break-words">{o().message}</p>
        )}
      </Show>
    </div>
  );
};

// ── Form primitives ──────────────────────────────────────────────────────────

const Field: Component<{
  label: string;
  hint?: string;
  children: JSX.Element;
}> = (props) => (
  <label class="block">
    <span class="block text-[10px] text-zinc-400 uppercase tracking-wider mb-1">
      {props.label}
    </span>
    {props.children}
    <Show when={props.hint}>
      <span class="block text-[9px] text-zinc-600 mt-1">{props.hint}</span>
    </Show>
  </label>
);

const inputClass =
  "w-full bg-zinc-900 border border-zinc-800 rounded px-2 py-1.5 text-[11px] text-zinc-200 placeholder:text-zinc-700 focus:outline-none focus:border-amber-500/60";

const SecretInput: Component<{
  value: string;
  onInput: (v: string) => void;
  alreadyConfigured: boolean;
  disabled?: boolean;
}> = (props) => (
  <input
    type="password"
    class={inputClass}
    value={props.value}
    placeholder={props.alreadyConfigured ? "•••••• (leave blank to keep)" : "(unset)"}
    disabled={props.disabled}
    onInput={(e) => props.onInput(e.currentTarget.value)}
    autocomplete="new-password"
  />
);

const FormActions: Component<{
  saving: boolean;
  canClear: boolean;
  envLocked: boolean;
  onClear: () => void;
}> = (props) => (
  <div class="flex items-center gap-2 mt-2">
    <button
      type="submit"
      class="text-[10px] uppercase tracking-wider px-3 py-1.5 bg-amber-500/20 text-amber-200 border border-amber-700/50 rounded hover:bg-amber-500/30 disabled:opacity-40"
      disabled={props.saving || props.envLocked}
    >
      {props.saving ? "saving…" : "save"}
    </button>
    <Show when={props.canClear && !props.envLocked}>
      <button
        type="button"
        onClick={props.onClear}
        class="text-[10px] uppercase tracking-wider px-3 py-1.5 text-zinc-500 hover:text-rose-400 border border-zinc-800 hover:border-rose-900 rounded"
      >
        clear
      </button>
    </Show>
    <Show when={props.envLocked}>
      <span class="text-[10px] text-zinc-600">
        Value is set via env var — remove from <code>.env</code> to edit here.
      </span>
    </Show>
  </div>
);

// ── UniFi Labels ─────────────────────────────────────────────────────────────

const UnifiLabelsSection: Component<{
  status: IntegrationStatus | undefined;
  onSave: (p: { url?: string | null; api_key?: string | null }) => Promise<void>;
  onClear: () => Promise<void>;
}> = (props) => {
  const initialUrl = () => (props.status?.plain?.url as string | undefined) ?? "";
  const [url, setUrl] = createSignal(initialUrl());
  const [apiKey, setApiKey] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const envLocked = () =>
    props.status?.plain_source === "env" || props.status?.secret_source === "env";

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      await props.onSave({
        url: url(),
        ...(apiKey() ? { api_key: apiKey() } : {}),
      });
      setApiKey("");
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    if (!confirm("Clear the UniFi labels integration? Devices keep their current names but stop refreshing.")) return;
    await props.onClear();
    setUrl("");
    setApiKey("");
  };

  return (
    <Section
      title="UniFi auto-label"
      description="Reads the Network Integration API every 60 seconds to populate device names. UniFi Network ≥ 9.0 with an Integrations API key."
      status={props.status}
    >
      <form onSubmit={submit} class="space-y-3">
        <Field
          label="Sites URL"
          hint="e.g. https://192.168.0.1/proxy/network/integration/v1/sites"
        >
          <input
            type="url"
            class={inputClass}
            value={url()}
            disabled={envLocked()}
            onInput={(e) => setUrl(e.currentTarget.value)}
            placeholder="https://<controller>/proxy/network/integration/v1/sites"
          />
        </Field>
        <Field
          label="API key"
          hint="UniFi → Settings → Control Plane → Integrations → Create API Key"
        >
          <SecretInput
            value={apiKey()}
            onInput={setApiKey}
            alreadyConfigured={!!props.status?.secret_configured}
            disabled={envLocked()}
          />
        </Field>
        <FormActions
          saving={saving()}
          envLocked={envLocked()}
          canClear={!!props.status?.plain_source || !!props.status?.secret_configured}
          onClear={() => void clear()}
        />
      </form>
    </Section>
  );
};

// ── UniFi IPS ────────────────────────────────────────────────────────────────

const UnifiIpsSection: Component<{
  status: IntegrationStatus | undefined;
  onSave: (p: {
    controller_url?: string | null;
    username?: string | null;
    password?: string | null;
    site?: string | null;
  }) => Promise<void>;
  onClear: () => Promise<void>;
}> = (props) => {
  const [controllerUrl, setControllerUrl] = createSignal(
    (props.status?.plain?.controller_url as string | undefined) ?? "",
  );
  const [username, setUsername] = createSignal(
    (props.status?.plain?.username as string | undefined) ?? "",
  );
  const [site, setSite] = createSignal(
    (props.status?.plain?.site as string | undefined) ?? "default",
  );
  const [password, setPassword] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const envLocked = () =>
    props.status?.plain_source === "env" || props.status?.secret_source === "env";

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      await props.onSave({
        controller_url: controllerUrl(),
        username: username(),
        site: site(),
        ...(password() ? { password: password() } : {}),
      });
      setPassword("");
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    if (
      !confirm(
        "Clear the UniFi IPS integration? You'll stop receiving Threat Management alerts in Lumen.",
      )
    )
      return;
    await props.onClear();
    setControllerUrl("");
    setUsername("");
    setSite("default");
    setPassword("");
  };

  return (
    <Section
      title="UniFi IPS alerts"
      description="Polls Threat Management alarms every 30 seconds and surfaces them as detection events. Legacy controller auth — needs a UniFi user with at least 'View Only' on Network."
      status={props.status}
    >
      <form onSubmit={submit} class="space-y-3">
        <Field label="Controller URL" hint="e.g. https://192.168.0.1 — no path">
          <input
            type="url"
            class={inputClass}
            value={controllerUrl()}
            disabled={envLocked()}
            onInput={(e) => setControllerUrl(e.currentTarget.value)}
            placeholder="https://192.168.0.1"
          />
        </Field>
        <div class="grid grid-cols-2 gap-3">
          <Field label="Username">
            <input
              type="text"
              class={inputClass}
              value={username()}
              disabled={envLocked()}
              onInput={(e) => setUsername(e.currentTarget.value)}
              placeholder="lumen-reader"
              autocomplete="username"
            />
          </Field>
          <Field label="Site" hint="Usually 'default'.">
            <input
              type="text"
              class={inputClass}
              value={site()}
              disabled={envLocked()}
              onInput={(e) => setSite(e.currentTarget.value)}
              placeholder="default"
            />
          </Field>
        </div>
        <Field label="Password">
          <SecretInput
            value={password()}
            onInput={setPassword}
            alreadyConfigured={!!props.status?.secret_configured}
            disabled={envLocked()}
          />
        </Field>
        <FormActions
          saving={saving()}
          envLocked={envLocked()}
          canClear={!!props.status?.plain_source || !!props.status?.secret_configured}
          onClear={() => void clear()}
        />
      </form>
    </Section>
  );
};

// ── Webhook ──────────────────────────────────────────────────────────────────

const WebhookSection: Component<{
  status: IntegrationStatus | undefined;
  onSave: (p: { url?: string | null }) => Promise<void>;
  onClear: () => Promise<void>;
}> = (props) => {
  const [url, setUrl] = createSignal((props.status?.plain?.url as string | undefined) ?? "");
  const [saving, setSaving] = createSignal(false);
  const envLocked = () => props.status?.plain_source === "env";

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      await props.onSave({ url: url() });
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    if (!confirm("Clear the detection webhook? No more outbound notifications.")) return;
    await props.onClear();
    setUrl("");
  };

  return (
    <Section
      title="Detection webhook"
      description="Every detection event is POSTed as JSON to this URL. Slack / Discord / n8n / Home Assistant / PagerDuty all accept it."
      status={props.status}
    >
      <form onSubmit={submit} class="space-y-3">
        <Field
          label="Webhook URL"
          hint="The URL itself is the secret — treat it as one. It is sent as the destination of every detection."
        >
          <input
            type="url"
            class={inputClass}
            value={url()}
            disabled={envLocked()}
            onInput={(e) => setUrl(e.currentTarget.value)}
            placeholder="https://hooks.slack.com/services/..."
          />
        </Field>
        <FormActions
          saving={saving()}
          envLocked={envLocked()}
          canClear={!!props.status?.plain_source}
          onClear={() => void clear()}
        />
      </form>
    </Section>
  );
};

// ── Users ────────────────────────────────────────────────────────────────────

const ROLE_OPTIONS: UserRole[] = ["admin", "operator", "viewer", "noc_display"];

const roleLabel = (id: UserRole, roles: RoleInfo[] | null | undefined): string =>
  roles?.find((r) => r.id === id)?.label ?? id;

const UsersSection: Component<{ store: ReturnType<typeof createUserAdminStore> }> = (
  props,
) => {
  const [showCreate, setShowCreate] = createSignal(false);
  const [editing, setEditing] = createSignal<UserDetail | null>(null);

  return (
    <section class="space-y-4">
      <header class="flex items-start justify-between gap-4">
        <div>
          <h3 class="text-sm font-semibold text-zinc-100">Users</h3>
          <p class="text-[10px] text-zinc-500 mt-1 max-w-prose">
            Local accounts. Roles bundle capabilities — see Roles &amp; capabilities for
            the matrix. Use OIDC to delegate authentication to an upstream IdP.
          </p>
        </div>
        <button
          type="button"
          class="text-[10px] uppercase tracking-wider px-3 py-1.5 bg-amber-500/20 text-amber-200 border border-amber-700/50 rounded hover:bg-amber-500/30"
          onClick={() => setShowCreate(true)}
        >
          + New user
        </button>
      </header>

      <Show when={props.store.error()}>
        {(err) => (
          <div class="px-3 py-2 text-[11px] text-rose-300 bg-rose-950/40 border border-rose-900/60 rounded">
            {err()}
          </div>
        )}
      </Show>

      <Show
        when={props.store.users() && props.store.users()!.length > 0}
        fallback={<p class="text-[10px] text-zinc-600">no users yet</p>}
      >
        <div class="border border-zinc-800/60 rounded overflow-hidden">
          <table class="w-full text-[11px]">
            <thead class="bg-zinc-900/50 text-[9px] uppercase tracking-wider text-zinc-500">
              <tr>
                <th class="text-left px-3 py-2 font-normal">Email</th>
                <th class="text-left px-3 py-2 font-normal">Role</th>
                <th class="text-left px-3 py-2 font-normal">Created</th>
                <th class="text-right px-3 py-2 font-normal w-20" />
              </tr>
            </thead>
            <tbody class="divide-y divide-zinc-800/60">
              <For each={props.store.users()!}>
                {(u) => (
                  <tr class="text-zinc-200 hover:bg-zinc-900/40">
                    <td class="px-3 py-2 font-mono truncate max-w-xs" title={u.email}>
                      {u.email}
                    </td>
                    <td class="px-3 py-2 text-zinc-400">
                      {roleLabel(u.role, props.store.roles()?.roles)}
                    </td>
                    <td class="px-3 py-2 text-zinc-500 tabular-nums">
                      {new Date(u.created_at_secs * 1000).toISOString().slice(0, 10)}
                    </td>
                    <td class="px-3 py-2 text-right">
                      <button
                        type="button"
                        class="text-[10px] text-zinc-500 hover:text-amber-300 px-2 py-0.5"
                        onClick={() => setEditing(u)}
                      >
                        edit
                      </button>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>

      <Show when={showCreate()}>
        <CreateUserModal
          roles={props.store.roles()?.roles ?? []}
          onClose={() => setShowCreate(false)}
          onSubmit={async (p) => {
            await props.store.create(p);
            setShowCreate(false);
          }}
        />
      </Show>

      <Show when={editing()}>
        {(u) => (
          <EditUserModal
            user={u()}
            roles={props.store.roles()?.roles ?? []}
            onClose={() => setEditing(null)}
            onSubmit={async (p) => {
              await props.store.update(u().id, p);
              setEditing(null);
            }}
            onDelete={async () => {
              if (
                !confirm(
                  `Delete user ${u().email}? Their active sessions will be revoked immediately.`,
                )
              )
                return;
              await props.store.remove(u().id);
              setEditing(null);
            }}
          />
        )}
      </Show>
    </section>
  );
};

const Modal: Component<{ title: string; onClose: () => void; children: JSX.Element }> = (
  props,
) => (
  <div
    class="fixed inset-0 z-[60] bg-zinc-950/85 backdrop-blur-sm flex items-center justify-center px-4"
    onClick={(e) => {
      if (e.target === e.currentTarget) props.onClose();
    }}
  >
    <div class="w-full max-w-md bg-zinc-950 border border-zinc-800 rounded-md shadow-2xl">
      <header class="flex items-center justify-between px-5 py-3 border-b border-zinc-800">
        <h4 class="text-[11px] font-semibold tracking-tight text-zinc-100">{props.title}</h4>
        <button
          type="button"
          onClick={props.onClose}
          class="text-zinc-500 hover:text-zinc-200 text-sm px-2 py-0.5"
        >
          ✕
        </button>
      </header>
      <div class="px-5 py-4">{props.children}</div>
    </div>
  </div>
);

const CreateUserModal: Component<{
  roles: RoleInfo[];
  onClose: () => void;
  onSubmit: (p: { email: string; password: string; role: UserRole }) => Promise<void>;
}> = (props) => {
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [role, setRole] = createSignal<UserRole>("viewer");
  const [saving, setSaving] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setErr(null);
    setSaving(true);
    try {
      await props.onSubmit({ email: email().trim(), password: password(), role: role() });
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal title="New user" onClose={props.onClose}>
      <form onSubmit={submit} class="space-y-3">
        <Field label="Email">
          <input
            class={inputClass}
            type="email"
            value={email()}
            onInput={(e) => setEmail(e.currentTarget.value)}
            placeholder="alice@example.com"
            required
            autofocus
          />
        </Field>
        <Field label="Initial password" hint="≥ 8 characters. Share out-of-band; the user changes it on next sign-in.">
          <input
            class={inputClass}
            type="password"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            autocomplete="new-password"
            minLength={8}
            required
          />
        </Field>
        <Field label="Role">
          <RoleSelect value={role()} onChange={setRole} roles={props.roles} />
        </Field>
        <Show when={err()}>
          {(e) => <p class="text-[10px] text-rose-300">{e()}</p>}
        </Show>
        <div class="flex justify-end gap-2 pt-2">
          <button
            type="button"
            class="text-[10px] uppercase tracking-wider px-3 py-1.5 text-zinc-500 hover:text-zinc-300 border border-zinc-800 rounded"
            onClick={props.onClose}
          >
            cancel
          </button>
          <button
            type="submit"
            class="text-[10px] uppercase tracking-wider px-3 py-1.5 bg-amber-500/20 text-amber-200 border border-amber-700/50 rounded hover:bg-amber-500/30 disabled:opacity-40"
            disabled={saving()}
          >
            {saving() ? "creating…" : "create"}
          </button>
        </div>
      </form>
    </Modal>
  );
};

const EditUserModal: Component<{
  user: UserDetail;
  roles: RoleInfo[];
  onClose: () => void;
  onSubmit: (p: { role?: UserRole; password?: string }) => Promise<void>;
  onDelete: () => Promise<void>;
}> = (props) => {
  const [role, setRole] = createSignal<UserRole>(props.user.role);
  const [password, setPassword] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setErr(null);
    const patch: { role?: UserRole; password?: string } = {};
    if (role() !== props.user.role) patch.role = role();
    if (password()) patch.password = password();
    if (Object.keys(patch).length === 0) {
      setErr("nothing to change");
      return;
    }
    setSaving(true);
    try {
      await props.onSubmit(patch);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal title={`Edit ${props.user.email}`} onClose={props.onClose}>
      <form onSubmit={submit} class="space-y-3">
        <Field label="Role">
          <RoleSelect value={role()} onChange={setRole} roles={props.roles} />
        </Field>
        <Field
          label="Reset password"
          hint="Leave blank to keep the current password. Setting one revokes every active session for this user."
        >
          <input
            class={inputClass}
            type="password"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            autocomplete="new-password"
            placeholder="(leave blank to keep)"
          />
        </Field>
        <Show when={err()}>
          {(e) => <p class="text-[10px] text-rose-300">{e()}</p>}
        </Show>
        <div class="flex items-center justify-between gap-2 pt-2">
          <button
            type="button"
            class="text-[10px] uppercase tracking-wider px-3 py-1.5 text-zinc-500 hover:text-rose-400 border border-zinc-800 hover:border-rose-900 rounded"
            onClick={() => void props.onDelete()}
          >
            delete
          </button>
          <div class="flex gap-2">
            <button
              type="button"
              class="text-[10px] uppercase tracking-wider px-3 py-1.5 text-zinc-500 hover:text-zinc-300 border border-zinc-800 rounded"
              onClick={props.onClose}
            >
              cancel
            </button>
            <button
              type="submit"
              class="text-[10px] uppercase tracking-wider px-3 py-1.5 bg-amber-500/20 text-amber-200 border border-amber-700/50 rounded hover:bg-amber-500/30 disabled:opacity-40"
              disabled={saving()}
            >
              {saving() ? "saving…" : "save"}
            </button>
          </div>
        </div>
      </form>
    </Modal>
  );
};

const RoleSelect: Component<{
  value: UserRole;
  onChange: (r: UserRole) => void;
  roles: RoleInfo[];
}> = (props) => (
  <select
    class={inputClass}
    value={props.value}
    onChange={(e) => props.onChange(e.currentTarget.value as UserRole)}
  >
    <For each={ROLE_OPTIONS}>
      {(id) => <option value={id}>{roleLabel(id, props.roles)}</option>}
    </For>
  </select>
);

// ── Roles & capabilities (read-only matrix) ──────────────────────────────────

const RolesSection: Component<{ store: ReturnType<typeof createUserAdminStore> }> = (
  props,
) => {
  const data = () => props.store.roles();

  return (
    <section class="space-y-4">
      <header>
        <h3 class="text-sm font-semibold text-zinc-100">Roles &amp; capabilities</h3>
        <p class="text-[10px] text-zinc-500 mt-1 max-w-prose">
          Each user has exactly one role. Roles bundle capabilities — the granular
          permissions checked at every gated endpoint. Custom roles aren&apos;t
          configurable yet; the four bundled roles cover the v1 use cases.
        </p>
      </header>

      <Show when={data()} fallback={<p class="text-[10px] text-zinc-600">loading…</p>}>
        {(d) => (
          <div class="space-y-5">
            <div class="border border-zinc-800/60 rounded overflow-x-auto">
              <table class="w-full text-[11px]">
                <thead class="bg-zinc-900/50 text-[9px] uppercase tracking-wider text-zinc-500">
                  <tr>
                    <th class="text-left px-3 py-2 font-normal sticky left-0 bg-zinc-900/50">
                      Capability
                    </th>
                    <For each={d().roles}>
                      {(r) => (
                        <th
                          class="text-center px-3 py-2 font-normal whitespace-nowrap"
                          title={r.description}
                        >
                          {r.label}
                        </th>
                      )}
                    </For>
                  </tr>
                </thead>
                <tbody class="divide-y divide-zinc-800/60">
                  <For each={d().capabilities}>
                    {(cap) => (
                      <tr class="text-zinc-300">
                        <td
                          class="px-3 py-2 font-mono text-zinc-400 sticky left-0 bg-zinc-950"
                          title={cap.description}
                        >
                          {cap.id}
                        </td>
                        <For each={d().roles}>
                          {(r) => (
                            <td class="px-3 py-2 text-center">
                              <Show
                                when={r.capabilities.includes(cap.id)}
                                fallback={<span class="text-zinc-700">·</span>}
                              >
                                <span class="text-emerald-400">●</span>
                              </Show>
                            </td>
                          )}
                        </For>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>

            <div class="space-y-2">
              <h4 class="text-[9px] uppercase tracking-wider text-zinc-500">
                Role descriptions
              </h4>
              <ul class="space-y-1 text-[10px] text-zinc-500">
                <For each={d().roles}>
                  {(r) => (
                    <li>
                      <span class="text-zinc-200">{r.label}:</span> {r.description}
                    </li>
                  )}
                </For>
              </ul>
            </div>
          </div>
        )}
      </Show>
    </section>
  );
};

// ── OIDC ─────────────────────────────────────────────────────────────────────

const DEFAULT_GROUP_MAPPING: OidcGroupMapping = { group: "", role: "viewer" };

const OidcSection: Component<{
  status: IntegrationStatus | undefined;
  onSave: (p: {
    issuer_url?: string | null;
    client_id?: string | null;
    client_secret?: string | null;
    group_mappings?: OidcGroupMapping[] | null;
  }) => Promise<void>;
  onClear: () => Promise<void>;
}> = (props) => {
  const initialIssuer = () =>
    (props.status?.plain?.issuer_url as string | undefined) ?? "";
  const initialClient = () =>
    (props.status?.plain?.client_id as string | undefined) ?? "";
  const initialMappings = (): OidcGroupMapping[] => {
    const raw = props.status?.plain?.group_mappings as unknown;
    return Array.isArray(raw) ? (raw as OidcGroupMapping[]) : [];
  };

  const [issuer, setIssuer] = createSignal(initialIssuer());
  const [clientId, setClientId] = createSignal(initialClient());
  const [clientSecret, setClientSecret] = createSignal("");
  const [mappings, setMappings] = createSignal<OidcGroupMapping[]>(initialMappings());
  const [saving, setSaving] = createSignal(false);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      await props.onSave({
        issuer_url: issuer(),
        client_id: clientId(),
        ...(clientSecret() ? { client_secret: clientSecret() } : {}),
        group_mappings: mappings(),
      });
      setClientSecret("");
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    if (
      !confirm(
        "Clear OIDC configuration? Users sign in with local passwords until it's reconfigured.",
      )
    )
      return;
    await props.onClear();
    setIssuer("");
    setClientId("");
    setClientSecret("");
    setMappings([]);
  };

  const addMapping = () =>
    setMappings((m) => [...m, { ...DEFAULT_GROUP_MAPPING }]);
  const removeMapping = (idx: number) =>
    setMappings((m) => m.filter((_, i) => i !== idx));
  const updateMapping = (idx: number, patch: Partial<OidcGroupMapping>) =>
    setMappings((m) => m.map((entry, i) => (i === idx ? { ...entry, ...patch } : entry)));

  return (
    <Section
      title="OIDC / single sign-on"
      description="Configure an OpenID Connect identity provider. Group claims map to Lumen roles — every user gets the role of the first group they belong to. The configuration is stored now; the actual sign-in flow lands in #14."
      status={props.status}
    >
      <div class="px-3 py-2 mb-3 text-[10px] text-sky-300 bg-sky-950/30 border border-sky-900/60 rounded">
        Configuration only — the OIDC sign-in flow ships with{" "}
        <a
          href="https://github.com/jamesagarside/lumen/issues/14"
          target="_blank"
          rel="noreferrer"
          class="underline hover:text-sky-200"
        >
          issue #14
        </a>
        . Settings entered here will be picked up automatically the moment that
        ships.
      </div>
      <form onSubmit={submit} class="space-y-3">
        <Field
          label="Issuer URL"
          hint="The OIDC issuer base URL — used for .well-known discovery. Trailing slash is stripped."
        >
          <input
            type="url"
            class={inputClass}
            value={issuer()}
            onInput={(e) => setIssuer(e.currentTarget.value)}
            placeholder="https://login.example.com/realms/lumen"
          />
        </Field>
        <Field label="Client ID">
          <input
            type="text"
            class={inputClass}
            value={clientId()}
            onInput={(e) => setClientId(e.currentTarget.value)}
            placeholder="lumen-web"
          />
        </Field>
        <Field
          label="Client secret"
          hint="Issued by the IdP when you register the application."
        >
          <SecretInput
            value={clientSecret()}
            onInput={setClientSecret}
            alreadyConfigured={!!props.status?.secret_configured}
          />
        </Field>

        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <span class="text-[10px] text-zinc-400 uppercase tracking-wider">
              Group → role mappings
            </span>
            <button
              type="button"
              class="text-[10px] text-zinc-500 hover:text-amber-300 px-2 py-0.5"
              onClick={addMapping}
            >
              + add mapping
            </button>
          </div>
          <p class="text-[9px] text-zinc-600">
            First match wins. A user with no matching group is refused sign-in rather
            than created with no role.
          </p>
          <Show
            when={mappings().length > 0}
            fallback={
              <p class="text-[10px] text-zinc-600 italic px-1">
                no group mappings — every authenticated user will be rejected
              </p>
            }
          >
            <ul class="space-y-1.5">
              <Index each={mappings()}>
                {(m, idx) => (
                  <li class="flex items-center gap-2">
                    <input
                      type="text"
                      class={inputClass + " flex-1"}
                      value={m().group}
                      onInput={(e) =>
                        updateMapping(idx, { group: e.currentTarget.value })
                      }
                      placeholder="upstream-group-name"
                    />
                    <span class="text-[10px] text-zinc-600">→</span>
                    <select
                      class={inputClass + " w-40"}
                      value={m().role}
                      onChange={(e) =>
                        updateMapping(idx, {
                          role: e.currentTarget.value as UserRole,
                        })
                      }
                    >
                      <For each={ROLE_OPTIONS}>
                        {(id) => <option value={id}>{id}</option>}
                      </For>
                    </select>
                    <button
                      type="button"
                      class="text-zinc-600 hover:text-rose-400 px-2"
                      onClick={() => removeMapping(idx)}
                      title="remove"
                    >
                      ✕
                    </button>
                  </li>
                )}
              </Index>
            </ul>
          </Show>
        </div>

        <FormActions
          saving={saving()}
          envLocked={false}
          canClear={!!props.status?.plain_source || !!props.status?.secret_configured}
          onClear={() => void clear()}
        />
      </form>
    </Section>
  );
};

export default AdminSettings;
