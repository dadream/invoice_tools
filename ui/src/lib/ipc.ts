import { invoke } from '@tauri-apps/api/core'

/** 与后端 `src-tauri/src/error.rs` 的 `ErrorKind` 一一对应，改动需同步两侧。 */
export type ErrorKind = 'database' | 'parse' | 'network' | 'io' | 'validation' | 'internal'

const ERROR_KINDS: readonly ErrorKind[] = ['database', 'parse', 'network', 'io', 'validation', 'internal']

export interface AppError {
  kind: ErrorKind
  message: string
  recoverable: boolean
}

export type IpcResult<T> = { ok: true; data: T } | { ok: false; error: AppError }

export function isAppError(value: unknown): value is AppError {
  if (typeof value !== 'object' || value === null) return false
  const candidate = value as Record<string, unknown>
  return (
    typeof candidate.kind === 'string' &&
    ERROR_KINDS.includes(candidate.kind as ErrorKind) &&
    typeof candidate.message === 'string' &&
    typeof candidate.recoverable === 'boolean'
  )
}

/** 把任意 reject 值收敛成 AppError，保证 UI 永远有可渲染的结构。 */
export function toAppError(value: unknown): AppError {
  if (isAppError(value)) return value
  const message = value instanceof Error ? value.message : String(value)
  return { kind: 'internal', message: `未预期的错误: ${message}`, recoverable: false }
}

/** invoke 的安全封装：不抛异常，错误统一为 AppError。 */
export async function invokeSafe<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<IpcResult<T>> {
  try {
    return { ok: true, data: await invoke<T>(command, args) }
  } catch (error) {
    return { ok: false, error: toAppError(error) }
  }
}

/** 面向用户的中文提示。可恢复的引导重试，不可恢复的引导查日志。 */
export function describeError(err: AppError): string {
  return err.recoverable
    ? `${err.message}（可稍后重试）`
    : `${err.message}（请查看日志: ~/.invoice-assistant/logs/）`
}
