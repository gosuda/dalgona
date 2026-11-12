<script lang="ts">
import StateTag from "#lib/components/StateTag.svelte";

interface Props {
	startedAtMs: number;
}

let { startedAtMs }: Props = $props();
let revision = $state(0);
</script>

<button type="button" onclick={() => (revision += 1)}>Revise</button>
<StateTag state="inProgress" {startedAtMs} tick={revision} />
