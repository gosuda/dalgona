<script lang="ts">
import {
	type DesktopNotifications,
	desktopNotificationTexts,
} from "#lib/notifications.ts";

interface Props {
	value: DesktopNotifications;
	onAllow: () => void;
}

let { value, onAllow }: Props = $props();
</script>

{#if value !== "unsupported"}
	<p class="desktop-notifications">
		<span>{desktopNotificationTexts[value]}</span>
		{#if value === "off"}
			<button class="quiet" type="button" onclick={onAllow}>Allow desktop notifications</button>
		{/if}
	</p>
{/if}

<style>
	.desktop-notifications {
		margin: 0;
		color: var(--dim);
		font-size: var(--size-small);
	}
</style>
