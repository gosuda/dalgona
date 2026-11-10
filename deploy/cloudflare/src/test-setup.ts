import { registerHooks } from "node:module";

registerHooks({
	resolve(specifier, context, next) {
		if (specifier === "@cloudflare/containers") {
			return {
				shortCircuit: true,
				url: "data:text/javascript,export class Container {}",
			};
		}
		return next(specifier, context);
	},
});
