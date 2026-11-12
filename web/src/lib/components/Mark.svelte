<script lang="ts">
interface Props {
	readonly size?: number;
}

let { size = 48 }: Props = $props();
const source = $derived(size < 24 ? "/dalgona-small.svg" : "/dalgona.svg");
</script>

<img
	class="mark"
	src={source}
	alt="dalgona: a caramel candy stamped with a crescent moon and the rabbit Gona"
	width={size}
	height={size}
/>

<style>
	.mark {
		display: block;
		margin-block-end: var(--space-3);
	}
</style>
