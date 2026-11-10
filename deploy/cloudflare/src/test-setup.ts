import { registerHooks } from "node:module";

registerHooks({
	resolve(specifier, context, next) {
		if (specifier === "@cloudflare/containers") {
			return {
				shortCircuit: true,
				url: "data:text/javascript,export class Container{constructor(ctx,env){this.ctx=ctx;this.env=env;this.envVars={};this.forwarded=[];}async fetch(request){this.forwarded.push(request);return new Response('container',{status:200});}async startAndWaitForPorts(){}async destroy(){}}",
			};
		}
		return next(specifier, context);
	},
});
