/**
 * i18next `context` for messages whose remediation differs per platform.
 *
 * Linux wording lives next to the base key as `<key>_linux`; the base key keeps the Windows
 * wording. Every other platform, and every key without a Linux variant, falls back to the base
 * key, so passing this context never hides a message.
 */
export function platformContext(
  userAgent: string = typeof navigator === "undefined" ? "" : navigator.userAgent,
): "linux" | undefined {
  return /\bLinux\b/.test(userAgent) && !/\bAndroid\b/.test(userAgent) ? "linux" : undefined;
}
