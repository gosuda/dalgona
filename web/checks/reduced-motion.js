(async () => {
	const failures = [];
	const elapsed = () => document.querySelector(".tag.live")?.textContent ?? "";
	const before = elapsed();
	const animations = document.getAnimations().length;
	if (animations !== 0) failures.push(`animations: ${animations} (want 0)`);
	await new Promise((resolve) => setTimeout(resolve, 3000));
	const after = elapsed();
	if (before === "") failures.push("no live tag: a turn must be running");
	if (after !== before) {
		failures.push(`elapsed text: "${after}" (want "${before}" unchanged)`);
	}
	return { ok: failures.length === 0, failures: failures.slice(0, 50) };
})();
