(async () => {
	const failures = [];
	const nav = document.querySelector('nav[aria-label="Conversation threads"]');
	if (!nav) return { ok: false, failures: ["nav: thread list not found"] };
	const shown = nav.querySelectorAll("li").length;
	if (shown < 40) failures.push(`threads: ${shown} shown (want 40)`);
	nav.scrollTop = nav.scrollHeight;
	if (nav.clientHeight > 288) failures.push(`nav: clientHeight ${nav.clientHeight} (want <= 288)`);
	if (nav.scrollTop === 0) failures.push("nav: does not scroll");
	const start = [...document.querySelectorAll(".sidebar button")].find((b) =>
		/^(Start thread|New thread)$/.test(b.textContent.trim()),
	);
	if (!start) {
		failures.push("start button: not found");
	} else {
		const box = start.getBoundingClientRect();
		if (box.top < 0 || box.bottom > window.innerHeight)
			failures.push(`start button: top ${Math.round(box.top)} bottom ${Math.round(box.bottom)} outside 0..${window.innerHeight}`);
	}
	return { ok: failures.length === 0, failures };
})()
