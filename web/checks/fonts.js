(async () => {
	const failures = [];
	const text = document.createElement("span");
	text.style.fontFamily = "var(--face-text)";
	text.textContent = "Aa 한글";
	const title = document.createElement("span");
	title.style.fontFamily = "var(--face-title)";
	title.textContent = "Aa";
	document.body.append(text, title);
	text.getBoundingClientRect();
	title.getBoundingClientRect();
	await document.fonts.ready;
	const samples = [
		["Hanken Grotesk Variable", "Aa"],
		["Bricolage Grotesque Variable", "Aa"],
		["Pretendard Variable", "한글"],
		["Symbols Nerd Subset", "\u{F0873}"],
	];
	for (const [family, sample] of samples)
		await document.fonts
			.load(`16px "${family}"`, sample)
			.catch((error) => failures.push(`${family}: ${error.message}`));
	await document.fonts.ready;
	text.remove();
	title.remove();

	const loaded = (family) =>
		[...document.fonts].filter(
			(face) =>
				face.family.replaceAll('"', "") === family && face.status === "loaded",
		);
	for (const [family] of samples) {
		if (loaded(family).length === 0)
			failures.push(`${family}: no face is loaded`);
	}

	const hangul = [0xac00, 0xd7a3];
	const coversHangul = (range) =>
		range.split(",").some((part) => {
			const [start, end = start] = part
				.trim()
				.replace(/^U\+/i, "")
				.split("-")
				.map((hex) => Number.parseInt(hex, 16));
			return start <= hangul[1] && end >= hangul[0];
		});
	for (const face of loaded("Pretendard Variable")) {
		if (!coversHangul(face.unicodeRange))
			failures.push(
				`Pretendard Variable face covers no Hangul: ${face.unicodeRange}`,
			);
	}
	return { ok: failures.length === 0, failures };
})();
