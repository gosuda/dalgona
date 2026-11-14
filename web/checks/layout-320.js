(() => {
	function elementLabel(el) {
		if (el.id) return `#${el.id}`;
		const cls = el.className
			? `.${String(el.className).trim().split(/\s+/)[0]}`
			: "";
		return `${el.tagName.toLowerCase()}${cls}`;
	}

	function checkScope(name, scope, limit, width, failures) {
		if (!scope) {
			failures.push(`${name}: not found`);
			return;
		}
		for (const el of [scope, ...scope.querySelectorAll("*")]) {
			const box = el.getBoundingClientRect();
			if (box.width === 0 || box.height === 0) continue;
			if (box.right > limit) {
				failures.push(
					`${name} ${elementLabel(el)}: right ${Math.round(box.right)} (want <= ${width})`,
				);
			}
		}
	}

	function checkOverlaps(failures) {
		const items = [...document.querySelectorAll(".band > .item")].filter(
			(el) => {
				const box = el.getBoundingClientRect();
				return box.width > 0 && box.height > 0;
			},
		);
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
	}

	function checkModelButton(failures) {
		const model = document.querySelector(".band .model-button");
		if (!model) {
			failures.push("model button: not found");
			return;
		}
		const style = getComputedStyle(model);
		const canvas = document.createElement("canvas").getContext("2d");
		canvas.font = style.font;
		const three = canvas.measureText("mmm").width;
		const shown = model.getBoundingClientRect().width;
		if (shown < three) {
			failures.push(
				`model button: ${Math.round(shown)}px wide (want >= ${Math.round(three)}px for 3 characters)`,
			);
		}
	}

	const failures = [];
	const width = window.innerWidth;
	const limit = width + 0.5;
	const root = document.documentElement;
	if (root.scrollWidth > width) {
		failures.push(
			`document: scrollWidth ${root.scrollWidth} (want <= ${width})`,
		);
	}
	checkScope("main", document.querySelector("main"), limit, width, failures);
	checkScope(
		"nav",
		document.querySelector('nav[aria-label="Conversation threads"]'),
		limit,
		width,
		failures,
	);
	checkScope("dock", document.querySelector(".dock"), limit, width, failures);
	checkOverlaps(failures);
	checkModelButton(failures);
	return { ok: failures.length === 0, failures: failures.slice(0, 50) };
})();
