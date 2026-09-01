/**
 * Tauri's raw IPC response is normally an ArrayBuffer. WebView/runtime upgrades and
 * test adapters may expose the same payload as a typed view or a JSON byte array.
 * Normalize all supported representations before handing the bytes to Blob/Image.
 */
export function normalizeIpcBytes(value: unknown): Uint8Array | null {
  if (value instanceof ArrayBuffer) return new Uint8Array(value)

  if (ArrayBuffer.isView(value)) {
    const view = value as ArrayBufferView
    return new Uint8Array(view.buffer, view.byteOffset, view.byteLength)
  }

  const candidate = Array.isArray(value)
    ? value
    : typeof value === 'object' && value !== null && Array.isArray((value as { data?: unknown }).data)
      ? (value as { data: unknown[] }).data
      : null
  if (!candidate || candidate.some((byte) => !Number.isInteger(byte) || Number(byte) < 0 || Number(byte) > 255)) {
    return null
  }
  return Uint8Array.from(candidate as number[])
}

export function isPngBytes(bytes: Uint8Array): boolean {
  const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
  return bytes.length > signature.length && signature.every((byte, index) => bytes[index] === byte)
}
