(async () => {
	const failures = [];
	const nav = document.querySelector('nav[aria-label="Conversation threads"]');
	if (!nav) return { ok: false, failures: ["nav: thread list not found"] };
	const shown = nav.querySelectorAll("li").length;
	if (shown < 40) failures.push(`threads: ${shown} shown (want 40)`);
	nav.scrollTop = nav.scrollHeight;
	if (nav.clientHeight > 288)
		failures.push(`nav: clientHeight ${nav.clientHeight} (want <= 288)`);
	if (nav.scrollTop === 0) failures.push("nav: does not scroll");
	const start = [...document.querySelectorAll(".sidebar button")].find((b) =>
		/^(Start thread|New thread)$/.test(b.textContent.trim()),
	);
	if (!start) {
		failures.push("start button: not found");
	} else {
		const box = start.getBoundingClientRect();
		if (box.top < 0 || box.bottom > window.innerHeight)
			failures.push(
				`start button: top ${Math.round(box.top)} bottom ${Math.round(box.bottom)} outside 0..${window.innerHeight}`,
			);
	}
	const buttons = [...nav.querySelectorAll("button.thread")];
	if (buttons.length === 0) failures.push("thread buttons: none found");
	for (const button of buttons) {
		nav.scrollTop = 0;
		if (document.activeElement instanceof HTMLElement)
			document.activeElement.blur();
		button.focus();
		await new Promise((resolve) => requestAnimationFrame(resolve));
		const buttonBox = button.getBoundingClientRect();
		const navBox = nav.getBoundingClientRect();
		if (buttonBox.top < navBox.top || buttonBox.bottom > navBox.bottom)
			failures.push(
				`thread button ${button.textContent.trim().slice(0, 30)}: ${Math.round(buttonBox.top)}..${Math.round(buttonBox.bottom)} outside nav ${Math.round(navBox.top)}..${Math.round(navBox.bottom)}`,
			);
	}
	for (const button of buttons) {
		nav.scrollTop = 0;
		if (document.activeElement instanceof HTMLElement)
			document.activeElement.blur();
		button.focus({ preventScroll: true });
		await new Promise((resolve) => requestAnimationFrame(resolve));
		if (nav.scrollTop !== 0)
			failures.push(
				`nav: scrollTop ${nav.scrollTop} after focus with preventScroll (want 0)`,
			);
	}
	return { ok: failures.length === 0, failures };
})();
