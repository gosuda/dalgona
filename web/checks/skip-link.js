(async () => {
	const failures = [];
	const threads = 'nav[aria-label="Conversation threads"] button.thread';
	const composer = () =>
		document.querySelector('[aria-label="Message composer"] textarea');
	const first = document.querySelector(threads);
	const current = document.querySelector(`${threads}[aria-current="true"]`);
	if (first && !current) {
		first.click();
		for (let i = 0; i < 100 && composer()?.disabled !== false; i++) {
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
	}
	const link = document.querySelector("a.skip-link");
	if (!link) return { ok: false, failures: ["skip link: not found"] };
	const transcript = document.getElementById("transcript");
	if (!transcript) return { ok: false, failures: ["transcript: not found"] };
	const viewport = `${innerWidth}x${innerHeight}`;
	if (viewport !== "1280x800")
		failures.push(
			`viewport: ${viewport} (want 1280x800; resize the window and run again)`,
		);
	else if (transcript.scrollHeight <= transcript.clientHeight)
		failures.push(
			`transcript: ${transcript.scrollHeight}px of content in a ${transcript.clientHeight}px column, want taller than the column; open a thread with a long transcript, such as long output`,
		);
	if (location.hash === "#transcript")
		history.replaceState(null, "", `${location.pathname}${location.search}`);
	if (document.activeElement instanceof HTMLElement)
		document.activeElement.blur();
	link.click();
	await new Promise((resolve) => requestAnimationFrame(resolve));
	if (document.activeElement?.id !== "transcript")
		failures.push(
			`skip link: focus is "${document.activeElement?.id ?? ""}" (want "transcript")`,
		);
	return { ok: failures.length === 0, failures };
})();
