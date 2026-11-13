const nodeNames: Readonly<Record<number, string>> = {
	3: "#text",
	8: "#comment",
	9: "#document",
	11: "#document-fragment",
};

Object.defineProperty(Node.prototype, "nodeName", {
	configurable: true,
	get(this: Node) {
		if (this.nodeType === Node.ELEMENT_NODE) return (this as Element).tagName;
		return nodeNames[this.nodeType] ?? "";
	},
});
