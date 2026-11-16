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
	window.scrollTo(0, 0);
	await new Promise((resolve) => requestAnimationFrame(() => resolve()));
	const height = document.documentElement.scrollHeight;
	if (height <= window.innerHeight)
		failures.push(
			`transcript: document ${height}px tall, want taller than ${window.innerHeight}px`,
		);
	const send = [...document.querySelectorAll(".dock button")].find(
		(b) => b.textContent.trim() === "Send",
	);
	if (!send) {
		failures.push("Send: not found in the dock");
	} else {
		const box = send.getBoundingClientRect();
		if (box.top < 0 || box.bottom > window.innerHeight)
			failures.push(
				`Send: top ${Math.round(box.top)} bottom ${Math.round(box.bottom)} outside 0..${window.innerHeight}`,
			);
	}
	return { ok: failures.length === 0, failures };
})();
