import { createSignal, type Accessor } from "solid-js";

export interface AuthUser {
  id: string;
  email: string;
  role: string;
}

export interface AuthMe {
  user: AuthUser;
  capabilities: string[];
}

export type AuthState =
  | { status: "loading" }
  | { status: "anonymous" }
  | { status: "authed"; me: AuthMe };

export interface AuthStore {
  state: Accessor<AuthState>;
  refresh: () => Promise<void>;
  login: (email: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
  can: (capability: string) => boolean;
}

const errorMessageFrom = async (res: Response): Promise<string> => {
  try {
    const body = (await res.json()) as { error?: string };
    return body.error ?? `HTTP ${res.status}`;
  } catch {
    return `HTTP ${res.status}`;
  }
};

export const createAuthStore = (): AuthStore => {
  const [state, setState] = createSignal<AuthState>({ status: "loading" });

  const refresh = async () => {
    try {
      const res = await fetch("/auth/me", { credentials: "include" });
      if (res.status === 401) {
        setState({ status: "anonymous" });
        return;
      }
      if (!res.ok) {
        setState({ status: "anonymous" });
        return;
      }
      const me = (await res.json()) as AuthMe;
      setState({ status: "authed", me });
    } catch {
      // Network / proxy errors — DaemonBanner handles the surfacing.
      setState({ status: "anonymous" });
    }
  };

  const login = async (email: string, password: string) => {
    const res = await fetch("/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "include",
      body: JSON.stringify({ email, password }),
    });
    if (!res.ok) {
      throw new Error(await errorMessageFrom(res));
    }
    const me = (await res.json()) as AuthMe;
    setState({ status: "authed", me });
  };

  const logout = async () => {
    try {
      await fetch("/auth/logout", { method: "POST", credentials: "include" });
    } catch {
      // Ignore network errors; we'll go anonymous either way.
    }
    setState({ status: "anonymous" });
  };

  const can = (capability: string): boolean => {
    const s = state();
    return s.status === "authed" && s.me.capabilities.includes(capability);
  };

  return { state, refresh, login, logout, can };
};
