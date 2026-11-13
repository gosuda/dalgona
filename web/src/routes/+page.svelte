<script lang="ts">
import { onMount } from "svelte";
import App from "#lib/components/App.svelte";
import { RpcClient } from "#lib/protocol.ts";
import { Session } from "#lib/session.svelte.ts";

const session = new Session((url, handlers) => new RpcClient(url, handlers));

onMount(() => {
	session.start(location);
	return () => session.stop();
});
</script>

<App {session} />
