(async () => {
	const failures = [];
	const dialog = document.querySelector("dialog[open]");
	if (!dialog) return { ok: false, failures: ["panel: not open"] };
	const head = dialog.querySelector(".panel-head");
	const body = dialog.querySelector(".panel-body");
	const foot = dialog.querySelector(".panel-foot");
	if (!head || !body)
		return { ok: false, failures: ["panel: head or body not found"] };
	const nextFrame = () =>
		new Promise((resolve) => requestAnimationFrame(resolve));
	const inside = (rect, frame) =>
		rect.top >= frame.top - 0.5 &&
		rect.bottom <= frame.bottom + 0.5 &&
		rect.left >= frame.left - 0.5 &&
		rect.right <= frame.right + 0.5;
	const check = (state) => {
		const fail = (text) => failures.push(`${state}: ${text}`);
		const frame = dialog.getBoundingClientRect();
		const view = { top: 0, bottom: innerHeight, left: 0, right: innerWidth };
		if (!inside(frame, view))
			fail(
				`dialog ${Math.round(frame.top)}..${Math.round(frame.bottom)} outside 0..${innerHeight}`,
			);
		if (dialog.scrollHeight > dialog.clientHeight + 1)
			fail(
				`dialog scrolls (scrollHeight ${dialog.scrollHeight} over clientHeight ${dialog.clientHeight}); only the body may scroll`,
			);
		if (
			body.scrollHeight > body.clientHeight + 1 &&
			!["auto", "scroll"].includes(getComputedStyle(body).overflowY)
		)
			fail("body is taller than its box but is not a scroll region");
		const title = head.querySelector("h2");
		if (!title || !inside(title.getBoundingClientRect(), frame))
			fail("title is not inside the dialog");
		const buttons = foot ? [...foot.querySelectorAll("button")] : [];
		for (const button of buttons) {
			const rect = button.getBoundingClientRect();
			const label = button.textContent.trim();
			if (!inside(rect, frame) || !inside(rect, view))
				fail(
					`${label}: ${Math.round(rect.top)}..${Math.round(rect.bottom)} is not fully inside the dialog and the viewport`,
				);
			const hit = document.elementFromPoint(
				rect.left + rect.width / 2,
				rect.top + rect.height / 2,
			);
			if (hit !== button && !button.contains(hit)) fail(`${label}: occluded`);
		}
	};
	check("open");
	const choices = [
		...dialog.querySelectorAll(
			'fieldset input[type="radio"][name$="-provider"]',
		),
	];
	for (const choice of choices) {
		choice.click();
		await nextFrame();
		await nextFrame();
		check(`provider ${choice.value}`);
	}
	return { ok: failures.length === 0, failures };
})();
