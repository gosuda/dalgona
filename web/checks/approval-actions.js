(async () => {
	const failures = [];
	const card = document.querySelector("section.request-card");
	if (!card) return { ok: false, failures: ["approval card: not shown"] };
	const body = card.querySelector('[aria-label="Request details"]');
	if (!body)
		return { ok: false, failures: ["request details: region not found"] };
	const viewport = `${innerWidth}x${innerHeight}`;
	if (!["320x568", "375x812", "667x375"].includes(viewport))
		failures.push(`viewport: ${viewport} (want 320x568, 375x812 or 667x375)`);
	const diffs = card.querySelectorAll("details.file-change").length;
	if (diffs < 3)
		failures.push(`diffs: ${diffs} shown (want at least 3 long diffs)`);
	const originalScroll = body.scrollTop;
	const dock = document.querySelector(".dock");
	const dockScroll = dock ? dock.scrollTop : 0;
	if (dock) dock.scrollTop = 0;
	const box = body.getBoundingClientRect();
	if (body.clientHeight < 36 || box.height < 36)
		failures.push(
			`request details: height ${body.clientHeight} (want readable height >= 36)`,
		);
	if (box.top < 0 || box.bottom > innerHeight)
		failures.push("request details: outside the viewport");
	if (!["auto", "scroll"].includes(getComputedStyle(body).overflowY))
		failures.push("request details: not a scroll region");
	body.scrollTop = body.scrollHeight;
	await new Promise((resolve) => requestAnimationFrame(resolve));
	if (body.scrollHeight > body.clientHeight && body.scrollTop === 0)
		failures.push("request details: did not scroll");
	const controls = [...card.querySelectorAll(".request-actions button")];
	if (controls.length !== 4)
		failures.push("request actions: want all four controls");
	for (const node of [card.querySelector("h2"), ...controls]) {
		if (!node) continue;
		const rect = node.getBoundingClientRect();
		const label = node.textContent.trim();
		if (
			rect.top < 0 ||
			rect.bottom > innerHeight ||
			rect.left < 0 ||
			rect.right > innerWidth
		)
			failures.push(`${label}: outside the viewport`);
		const hit = document.elementFromPoint(
			rect.left + rect.width / 2,
			rect.top + rect.height / 2,
		);
		if (hit !== node && !node.contains(hit))
			failures.push(`${label}: occluded`);
		if (node instanceof HTMLButtonElement && rect.height < 36)
			failures.push(`${label}: target height ${rect.height} (want >= 36)`);
	}
	const composer = document.querySelector('[aria-label="Message composer"]');
	const actions = card.querySelector(".request-actions");
	if (actions) {
		const cardBox = card.getBoundingClientRect();
		const actionsBox = actions.getBoundingClientRect();
		if (actionsBox.bottom > cardBox.bottom + 1)
			failures.push(
				`card: actions end ${Math.round(actionsBox.bottom - cardBox.bottom)}px below the card`,
			);
	}
	if (
		composer &&
		composer.getBoundingClientRect().top < card.getBoundingClientRect().bottom
	)
		failures.push("composer: overlaps approval evidence or actions");
	if (composer && actions && dock) {
		let covered = 0;
		for (const top of [0, dock.scrollHeight]) {
			dock.scrollTop = top;
			covered = Math.max(
				covered,
				actions.getBoundingClientRect().bottom -
					composer.getBoundingClientRect().top,
			);
		}
		if (covered > 1)
			failures.push(
				`composer: the approval actions cover its top ${Math.round(covered)}px`,
			);
	}
	if (document.documentElement.scrollWidth > innerWidth)
		failures.push("page: horizontal overflow");
	if (document.querySelectorAll(".dock .point").length !== 1)
		failures.push("dock: want one point");
	if (!dock) {
		failures.push("dock: not found");
	} else {
		const box = dock.getBoundingClientRect();
		if (box.top < 0 || box.bottom > innerHeight + 1)
			failures.push(
				`dock: ${Math.round(box.top)}..${Math.round(box.bottom)} outside 0..${innerHeight}`,
			);
		if (
			dock.scrollHeight > dock.clientHeight + 1 &&
			!["auto", "scroll"].includes(getComputedStyle(dock).overflowY)
		)
			failures.push(
				`dock: scrollHeight ${dock.scrollHeight} over clientHeight ${dock.clientHeight} but not a scroll region`,
			);
	}
	if (!composer) {
		failures.push("composer: not found");
	} else {
		if (dock) dock.scrollTop = dock.scrollHeight;
		const frame = composer.getBoundingClientRect();
		if (frame.top < 0 || frame.bottom > innerHeight + 1)
			failures.push(
				`composer: ${Math.round(frame.top)}..${Math.round(frame.bottom)} outside 0..${innerHeight}`,
			);
		if (
			composer.scrollHeight > composer.clientHeight + 1 &&
			!["auto", "scroll"].includes(getComputedStyle(composer).overflowY)
		)
			failures.push(
				`composer: scrollHeight ${composer.scrollHeight} over clientHeight ${composer.clientHeight} but not a scroll region`,
			);
		const composerScroll = composer.scrollTop;
		for (const node of composer.querySelectorAll("label, textarea, button")) {
			composer.scrollTop += node.getBoundingClientRect().top - frame.top;
			const rect = node.getBoundingClientRect();
			const label = node.textContent.trim() || node.tagName;
			if (rect.top < frame.top - 1 || rect.bottom > frame.bottom + 1) {
				failures.push(
					`composer: ${label} cannot scroll into view (${Math.round(rect.top)}..${Math.round(rect.bottom)} outside ${Math.round(frame.top)}..${Math.round(frame.bottom)})`,
				);
				continue;
			}
			const hit = document.elementFromPoint(
				rect.left + rect.width / 2,
				rect.top + rect.height / 2,
			);
			if (hit !== node && !node.contains(hit))
				failures.push(`composer: ${label} occluded`);
		}
		composer.scrollTop = composerScroll;
		const label = composer.querySelector("label");
		const field = composer.querySelector("textarea");
		if (label && field) {
			composer.scrollTop = 0;
			const box = composer.getBoundingClientRect();
			const rowEnd = field.getBoundingClientRect().bottom;
			if (
				label.getBoundingClientRect().top < box.top ||
				rowEnd > box.bottom + 1
			)
				failures.push(
					`composer: ${Math.round(box.height)}px tall, its label and field row are not both in view`,
				);
			const rowSlack = 4;
			const floor =
				rowEnd -
				box.top +
				parseFloat(getComputedStyle(composer).paddingBottom) +
				rowSlack;
			if (
				dock &&
				dock.scrollHeight > dock.clientHeight + 1 &&
				box.height > floor + 1
			)
				failures.push(
					`composer: ${Math.round(box.height)}px tall while the dock scrolls (want its label and one field row, ${Math.round(floor)}px)`,
				);
			composer.scrollTop = composerScroll;
		}
	}
	if (dock) dock.scrollTop = 0;
	const heading = card.querySelector("#request-title");
	const step = Math.max(48, body.clientHeight - 48);
	body.scrollTop = 0;
	if (!heading) {
		failures.push("request title: not found");
	} else {
		heading.focus();
		heading.dispatchEvent(
			new KeyboardEvent("keydown", {
				key: "PageDown",
				bubbles: true,
				cancelable: true,
			}),
		);
		await new Promise((resolve) => requestAnimationFrame(resolve));
		if (Math.abs(body.scrollTop - step) > 1)
			failures.push(
				`request details: PageDown moved ${Math.round(body.scrollTop)} (want ${Math.round(step)})`,
			);
	}
	body.scrollTop = 0;
	const inside = body.querySelector("pre");
	if (!inside) {
		failures.push("pre: not found");
	} else {
		inside.dispatchEvent(
			new KeyboardEvent("keydown", {
				key: "PageDown",
				bubbles: true,
				cancelable: true,
			}),
		);
		await new Promise((resolve) => requestAnimationFrame(resolve));
		if (body.scrollTop !== 0)
			failures.push(
				`request details: PageDown on the inner pre moved ${Math.round(body.scrollTop)} (want 0)`,
			);
	}
	body.scrollTop = originalScroll;
	if (dock) dock.scrollTop = dockScroll;
	return { ok: failures.length === 0, failures };
})();
