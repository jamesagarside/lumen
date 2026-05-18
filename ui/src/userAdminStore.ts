import { createSignal, type Accessor } from "solid-js";
import type { UserRole } from "./settingsStore";

export interface UserDetail {
  id: string;
  email: string;
  role: UserRole;
  /** Unix epoch seconds. */
  created_at_secs: number;
}

export interface CreateUserPayload {
  email: string;
  password: string;
  role: UserRole;
}

export interface UpdateUserPayload {
  role?: UserRole;
  /** Empty = unchanged. */
  password?: string;
}

export interface RoleInfo {
  id: UserRole;
  label: string;
  description: string;
  capabilities: string[];
}

export interface CapabilityInfo {
  id: string;
  description: string;
}

export interface RolesResponse {
  roles: RoleInfo[];
  capabilities: CapabilityInfo[];
}

export interface UserAdminStore {
  users: Accessor<UserDetail[] | null>;
  roles: Accessor<RolesResponse | null>;
  loading: Accessor<boolean>;
  error: Accessor<string | null>;
  /** Pulls both /admin/users and /admin/roles in parallel. */
  refresh: () => Promise<void>;
  create: (p: CreateUserPayload) => Promise<void>;
  update: (id: string, p: UpdateUserPayload) => Promise<void>;
  remove: (id: string) => Promise<void>;
}

const errorMessage = async (res: Response): Promise<string> => {
  try {
    const body = (await res.json()) as { error?: string };
    return body.error ?? `HTTP ${res.status}`;
  } catch {
    return `HTTP ${res.status}`;
  }
};

export const createUserAdminStore = (): UserAdminStore => {
  const [users, setUsers] = createSignal<UserDetail[] | null>(null);
  const [roles, setRoles] = createSignal<RolesResponse | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      const [usersRes, rolesRes] = await Promise.all([
        fetch("/admin/users", { credentials: "include" }),
        fetch("/admin/roles", { credentials: "include" }),
      ]);
      if (!usersRes.ok) throw new Error(await errorMessage(usersRes));
      if (!rolesRes.ok) throw new Error(await errorMessage(rolesRes));
      setUsers((await usersRes.json()) as UserDetail[]);
      setRoles((await rolesRes.json()) as RolesResponse);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  const create = async (p: CreateUserPayload) => {
    setError(null);
    const res = await fetch("/admin/users", {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(p),
    });
    if (!res.ok) {
      const msg = await errorMessage(res);
      setError(msg);
      throw new Error(msg);
    }
    await refresh();
  };

  const update = async (id: string, p: UpdateUserPayload) => {
    setError(null);
    // Strip empty password — server treats missing == leave alone, but
    // an empty string slipping into the payload would trip the "must
    // include a role or password" guard.
    const body: UpdateUserPayload = { ...p };
    if (!body.password) delete body.password;
    const res = await fetch(`/admin/users/${id}`, {
      method: "PATCH",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const msg = await errorMessage(res);
      setError(msg);
      throw new Error(msg);
    }
    await refresh();
  };

  const remove = async (id: string) => {
    setError(null);
    const res = await fetch(`/admin/users/${id}`, {
      method: "DELETE",
      credentials: "include",
    });
    if (!res.ok) {
      const msg = await errorMessage(res);
      setError(msg);
      throw new Error(msg);
    }
    await refresh();
  };

  return { users, roles, loading, error, refresh, create, update, remove };
};
