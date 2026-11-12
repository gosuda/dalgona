<script lang="ts">
import Panel from "#lib/components/panels/Panel.svelte";
import type { Session } from "#lib/session.svelte.ts";
import type { LoginMethod } from "#lib/types.ts";

interface Props {
	readonly session: Session;
	readonly open: boolean;
	readonly onClose: () => void;
}

let { session, open, onClose }: Props = $props();
const uid = $props.id();
let picked = $state<string | null>(null);
let browserMethod = $state<"browser" | "device">("browser");
let apiKey = $state("");
let formError = $state<string | null>(null);

const product = $derived(session.userAgent?.split("/")[0] || "dalgona");
const chosen = $derived(
	session.loginProviders.find((provider) => provider.id === picked) ?? null,
);
const flow = $derived(session.login);
const keyBusy = $derived(
	flow !== null && flow.method === "apiKey" && flow.pending,
);
const keyError = $derived(
	flow !== null && flow.method === "apiKey" && flow.provider === picked
		? flow.error
		: null,
);

$effect(() => {
	if (open) return;
	apiKey = "";
	formError = null;
	picked = null;
});

function submitKey(event: SubmitEvent): void {
	event.preventDefault();
	const provider = picked;
	if (provider === null) return;
	const key = apiKey.trim();
	if (key === "") {
		formError = "Enter the API key.";
		return;
	}
	formError = null;
	void session.startLogin(provider, "apiKey", key);
}

function startBrowser(provider: string): void {
	const method: LoginMethod = browserMethod;
	void session.startLogin(provider, method);
}

function copyCode(): void {
	const code = session.login?.userCode;
	if (code === null || code === undefined) return;
	void navigator.clipboard?.writeText(code).catch(() => undefined);
}

function close(): void {
	onClose();
}
</script>

<Panel title="Log in" {open} onClose={close}>
	{#snippet children()}
		{#if flow !== null && flow.method !== "apiKey"}
			{#if flow.method === "browser"}
				{#if flow.authUrl !== null}
					<p><a href={flow.authUrl} target="_blank" rel="noopener noreferrer">Open sign-in page</a></p>
				{:else}
					<p>Starting sign-in…</p>
				{/if}
			{:else if flow.verificationUrl !== null && flow.userCode !== null}
				<p>Open <a href={flow.verificationUrl} target="_blank" rel="noopener noreferrer">{flow.verificationUrl}</a> and enter this code:</p>
				<p><code>{flow.userCode}</code> <button type="button" class="quiet" onclick={copyCode}>Copy code</button></p>
			{:else}
				<p>Starting sign-in…</p>
			{/if}
			<p>Waiting for sign-in…</p>
			<button type="button" class="quiet" onclick={() => session.cancelLogin()}>Cancel</button>
		{:else}
			<fieldset>
				<legend>Provider</legend>
				{#each session.loginProviders as provider (provider.id)}
					<label>
						<input
							type="radio"
							name="{uid}-provider"
							value={provider.id}
							checked={picked === provider.id}
							onchange={() => (picked = provider.id)}
						/>
						<span>{provider.id}</span>
						<span>{provider.signedIn ? "Signed in" : "Not signed in"}</span>
					</label>
				{/each}
			</fieldset>
			{#if chosen !== null && chosen.login === "apiKey"}
				<form onsubmit={submitKey}>
					<label for="{uid}-key">API key for {chosen.id}</label>
					<input
						id="{uid}-key"
						type="password"
						autocomplete="off"
						spellcheck="false"
						bind:value={apiKey}
						disabled={keyBusy}
						aria-describedby="{uid}-error"
					/>
					{#if formError !== null || keyError !== null}
						<p id="{uid}-error" class="field-error">{formError ?? keyError}</p>
					{/if}
					<button type="submit" class="point" disabled={keyBusy}>Log in</button>
				</form>
			{:else if chosen !== null && chosen.login === "browser"}
				<fieldset>
					<legend>Method</legend>
					<label>
						<input type="radio" name="{uid}-method" value="browser" checked={browserMethod === "browser"} onchange={() => (browserMethod = "browser")} />
						<span>Browser</span>
					</label>
					<label>
						<input type="radio" name="{uid}-method" value="device" checked={browserMethod === "device"} onchange={() => (browserMethod = "device")} />
						<span>Device code</span>
					</label>
				</fieldset>
				<button type="button" class="point" onclick={() => startBrowser(chosen.id)}>Continue</button>
			{:else if chosen !== null}
				<p>{product} login {chosen.id}</p>
			{/if}
		{/if}
	{/snippet}
</Panel>
