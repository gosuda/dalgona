(async () => {
	const failures = [];
	const threads = 'nav[aria-label="Conversation threads"] button.thread';
	const composer = () =>
		document.querySelector('[aria-label="Message composer"] textarea');
	const frame = () =>
		new Promise((resolve) => requestAnimationFrame(() => resolve()));
	const first = document.querySelector(threads);
	const current = document.querySelector(`${threads}[aria-current="true"]`);
	if (first && !current) {
		first.click();
		for (let i = 0; i < 100 && composer()?.disabled !== false; i++) {
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
	}
	window.scrollTo(0, 0);
	await frame();
	const height = document.documentElement.scrollHeight;
	if (height <= window.innerHeight)
		failures.push(
			`transcript: document ${height}px tall, want taller than ${window.innerHeight}px`,
		);
	const dock = document.querySelector(".dock");
	const send = [...document.querySelectorAll(".dock button")].find(
		(b) => b.textContent.trim() === "Send",
	);
	if (!dock) failures.push("dock: not found");
	if (!send) failures.push("Send: not found in the dock");
	if (dock && send) {
		const narrow = matchMedia("(max-width: 52rem)").matches;
		const last = Math.max(0, height - window.innerHeight);
		const places = narrow
			? [
					["start", 0],
					["middle", Math.floor(last / 2)],
					["end", last],
				]
			: [["start", 0]];
		for (const [place, y] of places) {
			window.scrollTo(0, y);
			await frame();
			const where = `at the ${place} of the document, scroll ${Math.round(window.scrollY)}`;
			const sendBox = send.getBoundingClientRect();
			if (sendBox.top < 0 || sendBox.bottom > window.innerHeight)
				failures.push(
					`Send: top ${Math.round(sendBox.top)} bottom ${Math.round(sendBox.bottom)} outside 0..${window.innerHeight} ${where}`,
				);
			if (!narrow) continue;
			const dockBox = dock.getBoundingClientRect();
			if (Math.abs(dockBox.bottom - window.innerHeight) > 1)
				failures.push(
					`dock: bottom ${Math.round(dockBox.bottom)} (want ${window.innerHeight}) ${where}`,
				);
			if (dockBox.left > 1 || dockBox.right < window.innerWidth - 1)
				failures.push(
					`dock: spans ${Math.round(dockBox.left)}..${Math.round(dockBox.right)} (want 0..${window.innerWidth}) ${where}`,
				);
			if (place !== "end") continue;
			const blocks = [
				...document.querySelectorAll("#transcript .transcript-inner > *"),
			].filter((node) => node.getBoundingClientRect().height > 0);
			const lastBlock = blocks.at(-1);
			if (!lastBlock) {
				failures.push(
					"transcript: no row to compare with the dock; open a thread with output, such as long output",
				);
				continue;
			}
			const covered = lastBlock.getBoundingClientRect().bottom - dockBox.top;
			if (covered > 1)
				failures.push(
					`dock: covers the last transcript row by ${Math.round(covered)}px ${where}`,
				);
		}
		window.scrollTo(0, 0);
		await frame();
	}
	return { ok: failures.length === 0, failures };
})();
