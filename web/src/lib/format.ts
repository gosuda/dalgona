const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;

function pad(value: number): string {
	return value.toString().padStart(2, "0");
}

export function formatDuration(ms: number): string {
	if (ms < SECOND) return `${ms} ms`;
	if (ms < MINUTE) return `${Math.floor(ms / SECOND)}s`;
	if (ms < HOUR) {
		const minutes = Math.floor(ms / MINUTE);
		const seconds = Math.floor((ms % MINUTE) / SECOND);
		return `${minutes}m ${pad(seconds)}s`;
	}
	const hours = Math.floor(ms / HOUR);
	const minutes = Math.floor((ms % HOUR) / MINUTE);
	return `${hours}h ${pad(minutes)}m`;
}

export function formatRetryWait(ms: number): string {
	const minutes = Math.floor(ms / MINUTE);
	const seconds = Math.floor((ms % MINUTE) / SECOND);
	return `rate limited · retrying in ${minutes}m ${pad(seconds)}s · esc stops`;
}
