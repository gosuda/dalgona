<script module lang="ts">
import type { PanelKind } from "#lib/commands.ts";

export const threadPanelKinds: ReadonlySet<PanelKind> = new Set<PanelKind>([
	"resume",
	"new",
	"compact",
	"fork",
	"tree",
	"rewind",
	"advisor",
	"review",
]);
</script>

<script lang="ts">
import AdvisorPanel from "#lib/components/panels/AdvisorPanel.svelte";
import CompactPanel from "#lib/components/panels/CompactPanel.svelte";
import ForkPanel from "#lib/components/panels/ForkPanel.svelte";
import NewThreadPanel from "#lib/components/panels/NewThreadPanel.svelte";
import ResumePanel from "#lib/components/panels/ResumePanel.svelte";
import ReviewStartPanel from "#lib/components/panels/ReviewStartPanel.svelte";
import RewindPanel from "#lib/components/panels/RewindPanel.svelte";
import TreePanel from "#lib/components/panels/TreePanel.svelte";
import type { Session } from "#lib/session.svelte.ts";

interface Props {
	readonly session: Session;
}

let { session }: Props = $props();
</script>

<ResumePanel {session} open={session.panel === "resume"} />
<NewThreadPanel {session} open={session.panel === "new"} />
<CompactPanel {session} open={session.panel === "compact"} />
<ForkPanel {session} open={session.panel === "fork"} />
<TreePanel {session} open={session.panel === "tree"} />
<RewindPanel {session} open={session.panel === "rewind"} />
<AdvisorPanel {session} open={session.panel === "advisor"} />
<ReviewStartPanel {session} open={session.panel === "review"} />
