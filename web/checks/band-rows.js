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
	const band = document.querySelector(".status-band .band");
	if (!band) return { ok: false, failures: ["status band: not found"] };
	if (innerWidth <= 640)
		return {
			ok: false,
			failures: [
				`viewport: ${innerWidth}x${innerHeight} (want a window wider than 640px)`,
			],
		};
	const rowOf = {
		model: 0,
		effort: 1,
		approval: 1,
		sandbox: 1,
		folder: 1,
		queue: 1,
		context: 2,
		mark: 2,
	};
	const round = (value) => Math.round(value * 100) / 100;
	const rows = [];
	for (const item of band.querySelectorAll("[data-item]")) {
		const id = item.getAttribute("data-item");
		if (getComputedStyle(item).display === "none") {
			failures.push(
				`${id}: dropped at ${innerWidth}px (want every item shown in the three-row band)`,
			);
			continue;
		}
		if (item.getClientRects().length === 0) continue;
		if (!(id in rowOf)) {
			failures.push(`${id}: not an item of the band`);
			continue;
		}
		const content = [
			...item.querySelectorAll("button, .glyph, img, .percent, .count"),
		]
			.map((el) => el.getBoundingClientRect())
			.filter((box) => box.width > 0 && box.height > 0);
		if (content.length === 0) continue;
		const box = item.getBoundingClientRect();
		const row = rows[rowOf[id]] ?? {
			ids: [],
			right: -Infinity,
			top: Infinity,
			bottom: -Infinity,
		};
		row.ids.push(id);
		row.right = Math.max(row.right, ...content.map((c) => c.right));
		row.top = Math.min(row.top, box.top);
		row.bottom = Math.max(row.bottom, box.bottom);
		rows[rowOf[id]] = row;
	}
	const shown = rows.filter((row) => row !== undefined);
	if (shown.length < 2) {
		failures.push(
			`band: ${shown.length} row shown (want model, then effort approval sandbox folder queue, then context mark)`,
		);
	}
	const style = getComputedStyle(band);
	const edge =
		band.getBoundingClientRect().right -
		parseFloat(style.paddingRight) -
		parseFloat(style.borderRightWidth);
	for (const row of shown) {
		if (Math.abs(row.right - edge) > 0.5) {
			failures.push(
				`${row.ids.join(" ")}: row ends at ${round(row.right)} (want ${round(edge)}, the right edge of the band)`,
			);
		}
	}
	for (let i = 1; i < shown.length; i++) {
		const above = shown[i - 1];
		const below = shown[i];
		if (above.bottom > below.top + 0.5) {
			failures.push(
				`${above.ids.join(" ")}: row bottom ${round(above.bottom)} below the top ${round(below.top)} of ${below.ids.join(" ")} (want rows from top to bottom)`,
			);
		}
	}
	return { ok: failures.length === 0, failures };
})();
