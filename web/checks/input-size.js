(() => {
	const failures = [];
	const fields = [...document.querySelectorAll("input, textarea, select")];
	if (fields.length === 0)
		return { ok: false, failures: ["fields: none found"] };
	for (const field of fields) {
		const size = Number.parseFloat(getComputedStyle(field).fontSize);
		if (size < 16) {
			const label =
				field.id ||
				field.getAttribute("aria-label") ||
				field.name ||
				field.tagName.toLowerCase();
			failures.push(`${label}: font-size ${size}px (want >= 16px)`);
		}
	}
	return { ok: failures.length === 0, failures };
})();
