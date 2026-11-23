(() => {
	const failures = [];
	const forced = matchMedia("(forced-colors: active)").matches;
	const buttons = [...document.querySelectorAll("button.quiet")].filter(
		(el) =>
			el.getClientRects().length > 0 &&
			!el.matches(":hover, :active") &&
			el.getAnimations().length === 0,
	);
	if (buttons.length === 0) {
		return { ok: false, failures: ["quiet buttons: none in view"] };
	}
	const resolve = (property, token) => {
		if (getComputedStyle(document.body).getPropertyValue(token).trim() === "") {
			return `${token} is not defined`;
		}
		const probe = document.createElement("span");
		probe.style[property] = `var(${token})`;
		document.body.append(probe);
		const value = getComputedStyle(probe)[property];
		probe.remove();
		return value;
	};
	const typed = [...document.querySelectorAll("button")].filter(
		(el) =>
			el.getClientRects().length > 0 &&
			!el.matches(
				".thread, .thread-choice, .model-button, .request-actions button",
			),
	);
	const inFlight = (el) =>
		el.disabled || el.getAttribute("aria-disabled") === "true";
	const size = resolve("fontSize", "--size-label");
	const weight = resolve("fontWeight", "--weight-control");
	for (const el of typed) {
		const style = getComputedStyle(el);
		const name = `${el.textContent.trim()}${inFlight(el) ? " (disabled)" : ""}`;
		const want = (what, got, wanted, token) => {
			if (got !== wanted)
				failures.push(`${name}: ${what} ${got} (want ${token} ${wanted})`);
		};
		want("weight", style.fontWeight, weight, "--weight-control");
		want("size", style.fontSize, size, "--size-label");
		want("line height", style.lineHeight, style.fontSize, "--leading-tight");
		if (style.display !== "inline-flex" && style.display !== "flex")
			failures.push(`${name}: display ${style.display} (want inline flex)`);
		want("align", style.alignItems, "center", "centred");
		want("justify", style.justifyContent, "center", "centred");
		want("wrap", style.whiteSpace, "nowrap", "no wrap");
	}
	const clear = "rgba(0, 0, 0, 0)";
	for (const el of buttons) {
		const style = getComputedStyle(el);
		const label = `${el.textContent.trim()}${inFlight(el) ? " (disabled)" : ""}`;
		const fail = (text) => failures.push(`${label}: ${text}`);
		const want = (what, got, wanted, name) => {
			if (got !== wanted) fail(`${what} ${got} (want ${name} ${wanted})`);
		};
		const danger = el.classList.contains("danger");
		if (!danger) {
			want("padding left", style.paddingLeft, "12px", "12px");
			want("padding right", style.paddingRight, "12px", "12px");
		}
		if (forced) {
			if (style.borderTopStyle !== "solid" || style.borderTopWidth === "0px")
				fail(
					`border ${style.borderTopWidth} ${style.borderTopStyle} (want a visible border under forced colours)`,
				);
			continue;
		}
		if (inFlight(el)) {
			want(
				"fill",
				style.backgroundColor,
				resolve("backgroundColor", "--tone"),
				"--tone",
			);
			want(
				"edge",
				style.borderTopColor,
				resolve("borderTopColor", "--line"),
				"--line",
			);
			want("label", style.color, resolve("color", "--dim"), "--dim");
			continue;
		}
		want("fill", style.backgroundColor, clear, "no fill");
		if (danger) {
			const ink = resolve("color", "--failed-text");
			want("edge", style.borderTopColor, ink, "--failed-text");
			want("label", style.color, ink, "--failed-text");
			continue;
		}
		want("edge", style.borderTopColor, clear, "no edge");
		want("label", style.color, resolve("color", "--dim"), "--dim");
	}
	return { ok: failures.length === 0, failures };
})();
