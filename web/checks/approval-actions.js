(async () => {
	const failures = [];
	const card = document.querySelector("section.request-card");
	if (!card) return { ok: false, failures: ["approval card: not shown"] };
	const body = card.querySelector('[aria-label="Request details"]');
	if (!body) return { ok: false, failures: ["request details: region not found"] };
	body.scrollTop = body.scrollHeight;
	const accept = [...card.querySelectorAll(".request-actions button")].find(
		(button) => button.textContent.trim() === "Accept",
	);
	if (!accept) {
		failures.push("Accept: not found in the request actions");
	} else {
		const box = accept.getBoundingClientRect();
		if (box.top < 0 || box.bottom > window.innerHeight)
			failures.push(`Accept: top ${Math.round(box.top)} bottom ${Math.round(box.bottom)} outside 0..${window.innerHeight}`);
	}
	if (body.scrollTop === 0) failures.push("request details: did not scroll");
	return { ok: failures.length === 0, failures };
})()
