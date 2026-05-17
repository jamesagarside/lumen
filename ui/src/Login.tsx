import { createSignal, Show, type Component } from "solid-js";

interface Props {
  onLogin: (email: string, password: string) => Promise<void>;
}

const Login: Component<Props> = (props) => {
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [submitting, setSubmitting] = createSignal(false);

  const onSubmit = async (e: SubmitEvent) => {
    e.preventDefault();
    if (submitting()) return;
    setSubmitting(true);
    setError(null);
    try {
      await props.onLogin(email().trim(), password());
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <main class="h-screen bg-zinc-950 text-zinc-100 font-mono flex items-center justify-center">
      <form
        onSubmit={onSubmit}
        class="w-full max-w-sm space-y-4 p-6 border border-zinc-800 rounded bg-zinc-950"
      >
        <div class="space-y-1">
          <h1 class="text-lg font-semibold tracking-tight">Lumen</h1>
          <p class="text-[11px] text-zinc-500">Sign in to continue.</p>
        </div>

        <div class="space-y-2">
          <label class="block">
            <span class="text-[9px] uppercase tracking-wider text-zinc-600 block mb-1">
              Email
            </span>
            <input
              type="email"
              required
              autocomplete="email"
              value={email()}
              onInput={(e) => setEmail(e.currentTarget.value)}
              class="w-full bg-zinc-900 border border-zinc-800 rounded px-2 py-1.5 text-xs text-zinc-100 outline-none focus:border-amber-500/60"
            />
          </label>

          <label class="block">
            <span class="text-[9px] uppercase tracking-wider text-zinc-600 block mb-1">
              Password
            </span>
            <input
              type="password"
              required
              autocomplete="current-password"
              value={password()}
              onInput={(e) => setPassword(e.currentTarget.value)}
              class="w-full bg-zinc-900 border border-zinc-800 rounded px-2 py-1.5 text-xs text-zinc-100 outline-none focus:border-amber-500/60"
            />
          </label>
        </div>

        <Show when={error()}>
          {(err) => (
            <div class="text-[10px] text-rose-400 border border-rose-900/60 bg-rose-950/40 rounded px-2 py-1.5">
              {err()}
            </div>
          )}
        </Show>

        <button
          type="submit"
          disabled={submitting() || !email() || !password()}
          class="w-full py-1.5 text-[11px] uppercase tracking-wider bg-amber-500/15 border border-amber-500/40 text-amber-300 rounded disabled:opacity-40 disabled:cursor-not-allowed hover:bg-amber-500/25"
        >
          {submitting() ? "Signing in…" : "Sign in"}
        </button>

        <p class="text-[9px] text-zinc-700 text-center pt-2">
          first run? set <code class="text-zinc-500">LUMEN_INITIAL_ADMIN_EMAIL</code> and{" "}
          <code class="text-zinc-500">LUMEN_INITIAL_ADMIN_PASSWORD</code> on the daemon
        </p>
      </form>
    </main>
  );
};

export default Login;
