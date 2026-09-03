/**
 * 发票文件选择：把「系统对话框」与「窗口拖拽」两条路径都收在这一层，
 * 组件只调用这里，不直接碰 Tauri API。
 *
 * 为什么不用 `<input type="file">`：Tauri v2 的 webview 里 `File` 对象没有
 * `path` 属性（那不是浏览器 API），`input.files[0].path` 与
 * `dataTransfer.files[0].path` 都是 undefined。真实文件系统路径只能来自
 * plugin-dialog 的 `open()` 或 `onDragDropEvent` 的 `event.payload.paths`。
 */

import { open } from '@tauri-apps/plugin-dialog'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type { UnlistenFn } from '@tauri-apps/api/event'

/** 后端 `do_parse` 支持的扩展名，多一个都会被判 validation 错误。 */
export const SUPPORTED_EXTENSIONS = ['xml', 'ofd', 'pdf', 'png', 'jpg', 'jpeg', 'webp', 'bmp'] as const

/** 取小写扩展名；没有扩展名返回空串。 */
export function fileExtension(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? ''
  const dot = name.lastIndexOf('.')
  if (dot <= 0) return ''
  return name.slice(dot + 1).toLowerCase()
}

export function isSupportedInvoiceFile(path: string): boolean {
  return (SUPPORTED_EXTENSIONS as readonly string[]).includes(fileExtension(path))
}

/** 取文件名用于展示，避免整条绝对路径挤爆界面。 */
export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path
}

/**
 * 统计一组文件路径中实际保留的 OFD 原件数量。
 *
 * Windows 路径不区分大小写；同一路径可能同时作为主文件和关联材料返回，
 * 因此展示批次级提示前必须先去重。
 */
export function countUniqueOfdFiles(paths: string[]): number {
  return new Set(
    paths
      .filter((path) => fileExtension(path) === 'ofd')
      .map((path) => path.replaceAll('/', '\\').toLowerCase()),
  ).size
}

/**
 * 打开系统文件选择器，返回绝对路径；用户取消返回 null。
 *
 * `multiple: false` 时 `open()` 的返回类型是 `string | null`，
 * 这里仍做 typeof 收敛，防止上游类型推断变化后静默传入数组。
 */
export async function pickInvoiceFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: '发票文件', extensions: [...SUPPORTED_EXTENSIONS] }],
  })
  return typeof selected === 'string' ? selected : null
}

/** 拖拽状态：组件用它切换高亮，不必自己解析 payload.type。 */
export interface InvoiceDropHandlers {
  /** 拖入受支持的文件后触发，收到的是绝对路径数组 */
  onDrop: (paths: string[]) => void
  /** 拖拽悬停/离开，用于高亮 */
  onHover?: (hovering: boolean) => void
  /** 拖入的文件全部不受支持时触发，让界面能给出反馈而不是静默丢弃 */
  onUnsupported?: () => void
}

/**
 * 注册窗口级拖拽监听，返回 unlisten 函数。
 *
 * 调用方必须在组件卸载时（`$effect` 的 cleanup）调用返回的函数，
 * 否则每次重新渲染都会叠加一个监听，一次拖拽被处理多遍。
 */
export async function onInvoiceDrop(handlers: InvoiceDropHandlers): Promise<UnlistenFn> {
  const webview = getCurrentWebview()
  return await webview.onDragDropEvent((event) => {
    const payload = event.payload
    switch (payload.type) {
      case 'enter':
      case 'over':
        handlers.onHover?.(true)
        break
      case 'leave':
        handlers.onHover?.(false)
        break
      case 'drop': {
        handlers.onHover?.(false)
        const paths = payload.paths.filter(isSupportedInvoiceFile)
        if (paths.length > 0) {
          handlers.onDrop(paths)
        } else if (payload.paths.length > 0) {
          handlers.onUnsupported?.()
        }
        break
      }
    }
  })
}
