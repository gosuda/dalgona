<script lang="ts">
import "../app.css";
import {
	applyTheme,
	installTokens,
	parseThemeSetting,
	themeStorageKey,
} from "#lib/theme/theme.ts";

installTokens(document);
applyTheme(
	parseThemeSetting(localStorage.getItem(themeStorageKey)),
	document,
	localStorage,
);

let { children } = $props();
</script>

{@render children()}
