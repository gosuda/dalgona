(() => {
	const failures = [];
	const forced = matchMedia("(forced-colors: active)").matches;
	const tags = [...document.querySelectorAll(".tag, span.failed")].filter(
		(el) => el.getClientRects().length > 0,
	);
	if (tags.length === 0) {
		return { ok: false, failures: ["state tags: none in view"] };
	}
	const round = (value) => Math.round(value * 100) / 100;
	const where = (el) =>
		el.closest(".thread-head")
			? "header"
			: el.closest("nav")
				? "sidebar"
				: el.closest("article")
					? "card"
					: "page";
	const resolve = (context, property, token) => {
		if (getComputedStyle(context).getPropertyValue(token).trim() === "") {
			return `${token} is not defined`;
		}
		const probe = document.createElement("span");
		probe.style[property] = `var(${token})`;
		context.append(probe);
		const value = getComputedStyle(probe)[property];
		probe.remove();
		return value;
	};
	for (const el of tags) {
		const word = (el.querySelector(".word") ?? el).textContent.trim();
		const label = `${where(el)} ${word}`;
		const style = getComputedStyle(el);
		const box = el.getBoundingClientRect();
		const fail = (text) => failures.push(`${label}: ${text}`);
		const want = (what, got, wanted) => {
			if (got !== wanted) fail(`${what} ${got} (want ${wanted})`);
		};
		const isFailed = el.classList.contains("failed");
		const isLive = el.classList.contains("live");
		const isPoint = el.classList.contains("point");

		want("font size", style.fontSize, "13px");
		want("line height", style.lineHeight, style.fontSize);
		want("weight", style.fontWeight, isFailed ? "700" : "650");
		if (!/^"?Bricolage Grotesque/.test(style.fontFamily)) {
			fail(`face ${style.fontFamily} (want Bricolage Grotesque)`);
		}

		if (!forced) {
			const token = isFailed
				? "--failed-text"
				: word === "Offline"
					? "--warning-text"
					: isLive && isPoint
						? "--on-point"
						: "--dim";
			const ink = resolve(el.parentElement, "color", token);
			if (style.color !== ink) {
				fail(`colour ${style.color} (want ${token} ${ink})`);
			}
			const fill =
				isLive && isPoint
					? resolve(el.parentElement, "backgroundColor", "--point")
					: "rgba(0, 0, 0, 0)";
			want("fill", style.backgroundColor, fill);
		}

		if (isFailed) {
			want("border", style.borderTopWidth, "0px");
			continue;
		}

		const retry = el.textContent.includes("rate limited");
		if (retry ? box.height < 27.5 : Math.abs(box.height - 28) > 0.5) {
			fail(`height ${round(box.height)} (want ${retry ? "at least " : ""}28)`);
		}
		if (box.width < 83.5) fail(`width ${round(box.width)} (want at least 84)`);
		want("padding left", style.paddingLeft, "8px");
		want("padding right", style.paddingRight, "8px");
		for (const corner of [
			"borderTopLeftRadius",
			"borderTopRightRadius",
			"borderBottomLeftRadius",
			"borderBottomRightRadius",
		]) {
			want("radius", style[corner], "8px");
		}
		want("white space", style.whiteSpace, "nowrap");
		if (isLive) want("gap", style.columnGap, "8px");

		const column = el.parentElement.getBoundingClientRect();
		if (box.right > column.right + 0.5) {
			fail(
				`ends at ${round(box.right)} (want within ${round(column.right)}, its container)`,
			);
		}
		if (box.left < -0.5 || box.right > innerWidth + 0.5) {
			fail(
				`spans ${round(box.left)} to ${round(box.right)} (want within 0 to ${innerWidth})`,
			);
		}
		if (el.scrollWidth > el.clientWidth + 0.5) {
			fail(`content ${el.scrollWidth} wide in a box of ${el.clientWidth}`);
		}
	}
	return {
		ok: failures.length === 0,
		failures: [...new Set(failures)].slice(0, 50),
	};
})();
