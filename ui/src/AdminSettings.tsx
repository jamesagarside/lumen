import {
  createSignal,
  For,
  Match,
  onMount,
  Show,
  Switch,
  type Component,
  type JSX,
} from "solid-js";
import {
  createSettingsStore,
  type IntegrationId,
  type IntegrationStatus,
} from "./settingsStore";

interface AdminSettingsProps {
  onClose: () => void;
}

/// Admin → Settings overlay. Modal-style: covers the screen, dimmed
/// background, ESC to close. Mounts the settings store on open and
/// fetches the integration status; each section component handles
/// its own form state.
const AdminSettings: Component<AdminSettingsProps> = (props) => {
  const store = createSettingsStore();

  onMount(() => {
    void store.refresh();
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
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
      <div class="w-full max-w-3xl bg-zinc-950 border border-zinc-800 rounded-md shadow-2xl">
        <header class="flex items-center justify-between px-6 py-4 border-b border-zinc-800">
          <div>
            <h2 class="text-sm font-semibold tracking-tight text-zinc-100">Settings</h2>
            <p class="text-[10px] text-zinc-500 mt-0.5">
              Integration credentials and webhooks. Secrets are encrypted at rest.
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

        <div class="px-6 py-4 space-y-6">
          <Switch>
            <Match when={store.loading() && !store.statuses()}>
              <p class="text-[10px] text-zinc-600">loading…</p>
            </Match>
            <Match when={store.statuses()}>
              <UnifiLabelsSection
                status={byId("unifi_labels")}
                onSave={store.saveUnifiLabels}
                onClear={() => store.clear("unifi_labels")}
              />
              <UnifiIpsSection
                status={byId("unifi_ips")}
                onSave={store.saveUnifiIps}
                onClear={() => store.clear("unifi_ips")}
              />
              <WebhookSection
                status={byId("webhook")}
                onSave={store.saveWebhook}
                onClear={() => store.clear("webhook")}
              />
            </Match>
          </Switch>
        </div>

        <footer class="px-6 py-3 border-t border-zinc-800 text-[10px] text-zinc-600 flex items-center justify-between">
          <span>
            Changes take effect immediately — pollers restart in place.
          </span>
          <Show when={store.loading() && store.statuses()}>
            <span class="text-amber-400">refreshing…</span>
          </Show>
        </footer>
      </div>
    </div>
  );
};

// ── Section shell ────────────────────────────────────────────────────────────

interface SectionProps {
  title: string;
  description: string;
  status: IntegrationStatus | undefined;
  children: JSX.Element;
}

const Section: Component<SectionProps> = (props) => (
  <section class="border border-zinc-800/80 rounded-md">
    <header class="px-4 py-3 border-b border-zinc-800/80 flex items-start justify-between gap-4">
      <div>
        <h3 class="text-[11px] font-semibold tracking-tight text-zinc-200 uppercase">
          {props.title}
        </h3>
        <p class="text-[10px] text-zinc-500 mt-1">{props.description}</p>
      </div>
      <StatusBadges status={props.status} />
    </header>
    <div class="px-4 py-4">{props.children}</div>
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
    // Distinguish env-sourced (read-only) from db-sourced configuration.
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

// ── Reusable form bits ───────────────────────────────────────────────────────

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
  <div class="flex items-center gap-2 mt-4">
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
        // Omit the API key field entirely when the user left it blank
        // — otherwise we'd clear the existing one. The backend treats
        // empty as "clear" and missing as "keep current".
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
      description="Reads the Network Integration API every 60s to populate device names. UniFi Network ≥ 9.0."
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
      description="Polls Threat Management alarms every 30s and surfaces them as detection events. Legacy controller auth (username + password)."
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
          <Field
            label="Username"
            hint="Local UniFi user with at least 'View Only' on Network."
          >
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

export default AdminSettings;
