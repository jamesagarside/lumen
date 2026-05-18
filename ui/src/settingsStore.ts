import { createSignal, type Accessor } from "solid-js";

/// Mirror of `IntegrationStatusResponse` on the Rust side. The `plain`
/// object is loosely-typed because each integration carries different
/// fields (controller_url / username / site / etc.); the section
/// components narrow it when rendering.
export type IntegrationId = "unifi_labels" | "unifi_ips" | "webhook";

export type ConfigSource = "db" | "env";

export interface IntegrationStatus {
  id: IntegrationId;
  plain: Record<string, string | null | undefined>;
  plain_source: ConfigSource | null;
  secret_configured: boolean;
  secret_source: ConfigSource | null;
  running: boolean;
}

export interface UnifiLabelsPayload {
  /** Empty string clears the URL. */
  url?: string | null;
  /** Omitted = leave existing in place; empty = clear. */
  api_key?: string | null;
}

export interface UnifiIpsPayload {
  controller_url?: string | null;
  username?: string | null;
  password?: string | null;
  site?: string | null;
}

export interface WebhookPayload {
  url?: string | null;
}

export interface SettingsStore {
  statuses: Accessor<IntegrationStatus[] | null>;
  loading: Accessor<boolean>;
  error: Accessor<string | null>;
  /** Initial load + post-mutation reloads. */
  refresh: () => Promise<void>;
  saveUnifiLabels: (p: UnifiLabelsPayload) => Promise<void>;
  saveUnifiIps: (p: UnifiIpsPayload) => Promise<void>;
  saveWebhook: (p: WebhookPayload) => Promise<void>;
  clear: (id: IntegrationId) => Promise<void>;
}

const errorMessage = async (res: Response): Promise<string> => {
  try {
    const body = (await res.json()) as { error?: string };
    return body.error ?? `HTTP ${res.status}`;
  } catch {
    return `HTTP ${res.status}`;
  }
};

export const createSettingsStore = (): SettingsStore => {
  const [statuses, setStatuses] = createSignal<IntegrationStatus[] | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await fetch("/admin/settings", { credentials: "include" });
      if (!res.ok) {
        throw new Error(await errorMessage(res));
      }
      const body = (await res.json()) as IntegrationStatus[];
      setStatuses(body);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
    } finally {
      setLoading(false);
    }
  };

  const put = async (path: string, payload: unknown) => {
    setError(null);
    const res = await fetch(path, {
      method: "PUT",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    if (!res.ok) {
      const msg = await errorMessage(res);
      setError(msg);
      throw new Error(msg);
    }
    await refresh();
  };

  return {
    statuses,
    loading,
    error,
    refresh,
    saveUnifiLabels: (p) => put("/admin/settings/unifi_labels", p),
    saveUnifiIps: (p) => put("/admin/settings/unifi_ips", p),
    saveWebhook: (p) => put("/admin/settings/webhook", p),
    clear: async (id) => {
      setError(null);
      const res = await fetch(`/admin/settings/${id}`, {
        method: "DELETE",
        credentials: "include",
      });
      if (!res.ok) {
        const msg = await errorMessage(res);
        setError(msg);
        throw new Error(msg);
      }
      await refresh();
    },
  };
};
