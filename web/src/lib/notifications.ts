import { stateWords } from "./labels";
import type { TurnStatus } from "./types";

export type AttentionKind = "waiting" | "completed" | "failed" | "interrupted";

export type DesktopNotifications = "on" | "blocked" | "off" | "unsupported";

export const attentionTexts: Readonly<Record<AttentionKind, string>> = {
	waiting: "Waiting for your reply",
	completed: stateWords.completed,
	failed: stateWords.failed,
	interrupted: stateWords.interrupted,
};

export const desktopNotificationTexts: Readonly<
	Record<Exclude<DesktopNotifications, "unsupported">, string>
> = {
	on: "Desktop notifications are on",
	blocked: "Desktop notifications are blocked in this browser",
	off: "Desktop notifications are off",
};

export function attentionOf(status: TurnStatus): AttentionKind | null {
	return status === "inProgress" ? null : status;
}

export function notificationPermission(): string | undefined {
	return typeof Notification === "undefined"
		? undefined
		: Notification.permission;
}

export function desktopState(
	permission: string | undefined,
): DesktopNotifications {
	if (permission === undefined) return "unsupported";
	if (permission === "granted") return "on";
	if (permission === "denied") return "blocked";
	return "off";
}

export function visibilityState(): string | undefined {
	return typeof document === "undefined" ? undefined : document.visibilityState;
}
