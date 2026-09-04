// @vitest-environment jsdom

import { mount, tick, unmount } from 'svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { invokeSafe } from '../../lib/ipc'
import ConcurCapabilitySettings from './ConcurCapabilitySettings.svelte'

vi.mock('../../lib/ipc', () => ({
  invokeSafe: vi.fn(),
  describeError: vi.fn(() => '测试失败'),
}))

const invokeMock = vi.mocked(invokeSafe)
let mounted: ReturnType<typeof mount> | null = null

async function settle() {
  for (let index = 0; index < 5; index += 1) {
    await Promise.resolve()
    await tick()
  }
}

beforeEach(() => {
  invokeMock.mockImplementation(async (command) => {
    if (command === 'get_concur_connection_status') {
      return {
        ok: true,
        data: {
          configured: false,
          base_url: null,
          read_verified: false,
          draft_workflow_verified: false,
          verified_at: null,
          authorization_method: null,
          granted_scopes: [],
          connected_account: null,
          capability_checks: [],
          reason: '尚未连接',
        },
      } as never
    }
    if (command === 'get_concur_browser_oauth_config') {
      return {
        ok: true,
        data: {
          redirect_uri: 'http://127.0.0.1:53682/concur/oauth/callback',
          scopes: 'EXPRPT IMAGE',
          timeout_seconds: 300,
        },
      } as never
    }
    if (command === 'test_concur_browser_oauth') {
      return {
        ok: true,
        data: {
          success: true,
          checked_at: '2026-09-04T12:00:00Z',
          draft_report_id: null,
          draft_report_name: null,
          connected_account: { display_name: '测试用户', login_id: 'alpha@example.test' },
          steps: [
            { key: 'browser_authorization', label: '浏览器登录与授权回调', status: 'passed', message: '通过' },
            { key: 'report_read', label: '读取当前账号报销单', status: 'passed', message: '通过' },
          ],
          next_action: '继续测试',
        },
      } as never
    }
    throw new Error(`unexpected command: ${command}`)
  })
})

afterEach(async () => {
  if (mounted) await unmount(mounted)
  mounted = null
  invokeMock.mockReset()
  document.body.innerHTML = ''
})

describe('ConcurCapabilitySettings', () => {
  it('shows the four capability checks and browser OAuth boundary', async () => {
    mounted = mount(ConcurCapabilitySettings, { target: document.body })
    await settle()

    expect(document.body.textContent).toContain('连接 Concur 账号')
    expect(document.body.textContent).toContain('报销单读取')
    expect(document.body.textContent).toContain('草稿创建')
    expect(document.body.textContent).toContain('费用创建')
    expect(document.body.textContent).toContain('发票上传')
    expect(document.body.textContent).toContain('http://127.0.0.1:53682/concur/oauth/callback')
  })

  it('clears the client secret immediately after starting browser authorization', async () => {
    mounted = mount(ConcurCapabilitySettings, { target: document.body })
    await settle()

    const clientId = document.querySelector<HTMLInputElement>('input[placeholder="企业测试 OAuth 应用 ID"]')!
    const clientSecret = document.querySelector<HTMLInputElement>('input[placeholder="只用于本次授权码交换"]')!
    const confirmation = document.querySelector<HTMLInputElement>('.test-section .confirmation input')!
    clientId.value = 'synthetic-client-id'
    clientId.dispatchEvent(new Event('input', { bubbles: true }))
    clientSecret.value = 'synthetic-client-secret'
    clientSecret.dispatchEvent(new Event('input', { bubbles: true }))
    confirmation.checked = true
    confirmation.dispatchEvent(new Event('change', { bubbles: true }))
    await tick()

    const button = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find((item) => item.textContent?.includes('打开系统浏览器并测试授权'))!
    button.click()
    await settle()

    expect(invokeMock).toHaveBeenCalledWith('test_concur_browser_oauth', {
      input: {
        base_url: 'https://cn.api.concurcdc.cn',
        client_id: 'synthetic-client-id',
        client_secret: 'synthetic-client-secret',
        confirmed: true,
      },
    })
    expect(clientSecret.value).toBe('')
  })

  it('shows the connected account and saved pass or failure for each capability', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'get_concur_connection_status') {
        return {
          ok: true,
          data: {
            configured: true,
            base_url: 'https://cn.api.concurcdc.cn',
            read_verified: true,
            draft_workflow_verified: false,
            verified_at: '2026-09-04T12:00:00Z',
            authorization_method: 'browser_oauth',
            granted_scopes: ['EXPRPT', 'IMAGE'],
            connected_account: { display_name: 'Alpha 用户', login_id: 'alpha@example.test' },
            capability_checks: [
              { key: 'report_read', label: '读取当前账号报销单', status: 'passed', message: '可以读取报销单' },
              { key: 'report_create', label: '创建未提交测试草稿', status: 'passed', message: '草稿已创建' },
              { key: 'report_readback', label: '回读草稿状态', status: 'passed', message: '状态为未提交' },
              { key: 'expense_create', label: '创建测试费用', status: 'failed', message: '费用类型不可用' },
            ],
            reason: '部分能力待验证',
          },
        } as never
      }
      if (command === 'get_concur_browser_oauth_config') {
        return { ok: true, data: { redirect_uri: 'http://127.0.0.1:53682/concur/oauth/callback', scopes: 'EXPRPT IMAGE', timeout_seconds: 300 } } as never
      }
      throw new Error(`unexpected command: ${command}`)
    })

    mounted = mount(ConcurCapabilitySettings, { target: document.body })
    await settle()

    expect(document.body.textContent).toContain('Alpha 用户')
    expect(document.body.textContent).toContain('alpha@example.test')
    const cards = Array.from(document.querySelectorAll<HTMLElement>('.capability-grid article'))
    expect(cards.map((card) => card.textContent)).toEqual(expect.arrayContaining([
      expect.stringContaining('报销单读取'),
      expect.stringContaining('草稿创建'),
      expect.stringContaining('费用创建'),
      expect.stringContaining('发票上传'),
    ]))
    expect(cards[0].textContent).toContain('通过')
    expect(cards[1].textContent).toContain('通过')
    expect(cards[2].textContent).toContain('失败')
    expect(cards[3].textContent).toContain('未测试')
  })
})
