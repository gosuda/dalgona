(() => {
	const failures = [];
	const width = window.innerWidth;
	const limit = width + 0.5;
	const root = document.documentElement;
	if (root.scrollWidth > width)
		failures.push(
			`document: scrollWidth ${root.scrollWidth} (want <= ${width})`,
		);
	const scopes = [
		["main", document.querySelector("main")],
		["nav", document.querySelector('nav[aria-label="Conversation threads"]')],
		["dock", document.querySelector(".dock")],
	];
	for (const [name, scope] of scopes) {
		if (!scope) {
			failures.push(`${name}: not found`);
			continue;
		}
		for (const el of [scope, ...scope.querySelectorAll("*")]) {
			const box = el.getBoundingClientRect();
			if (box.width === 0 || box.height === 0) continue;
			if (box.right > limit) {
				const label = el.id
					? `#${el.id}`
					: `${el.tagName.toLowerCase()}${el.className ? `.${String(el.className).trim().split(/\s+/)[0]}` : ""}`;
				failures.push(
					`${name} ${label}: right ${Math.round(box.right)} (want <= ${width})`,
				);
			}
		}
	}
	const items = [...document.querySelectorAll(".band > .item")].filter((el) => {
		const box = el.getBoundingClientRect();
		return box.width > 0 && box.height > 0;
	});
	for (let i = 0; i < items.length; i++) {
		for (let j = i + 1; j < items.length; j++) {
			const a = items[i].getBoundingClientRect();
			const b = items[j].getBoundingClientRect();
			const overlapX = Math.min(a.right, b.right) - Math.max(a.left, b.left);
			const overlapY = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
			if (overlapX > 0.5 && overlapY > 0.5) {
				failures.push(
					`band ${items[i].dataset.item} overlaps ${items[j].dataset.item}: ${Math.round(overlapX)}x${Math.round(overlapY)}px`,
				);
			}
		}
	}
	const model = document.querySelector(".band .model-button");
	if (!model) {
		failures.push("model button: not found");
	} else {
		const style = getComputedStyle(model);
		const canvas = document.createElement("canvas").getContext("2d");
		canvas.font = style.font;
		const three = canvas.measureText("mmm").width;
		const shown = model.getBoundingClientRect().width;
		if (shown < three)
			failures.push(
				`model button: ${Math.round(shown)}px wide (want >= ${Math.round(three)}px for 3 characters)`,
			);
	}
	return { ok: failures.length === 0, failures: failures.slice(0, 50) };
})();
