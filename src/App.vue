<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import gsap from 'gsap'
import {
  cancelDecrypt,
  executeDecrypt,
  importFile,
  onProgress,
  pingGrpc,
  previewDecrypt,
} from './api'
import {
  DEFAULT_GRPC_CONFIG,
  type DecodeMode,
  type ExecuteStats,
  type ImportPreview,
  type PreviewItem,
  type ProgressPayload,
} from './types'

type Step = 'import' | 'executing' | 'done'

const step = ref<Step>('import')
const loading = ref('')
const error = ref('')
const filePath = ref('')

const preview = ref<ImportPreview | null>(null)
const mode = ref<DecodeMode>('auto')
const outputColumnName = ref('解密结果')
const grpcConfig = ref({ ...DEFAULT_GRPC_CONFIG })
const pingState = ref<{ status: 'idle' | 'testing' | 'ok' | 'fail'; message: string }>({
  status: 'idle',
  message: '',
})

const previewItems = ref<PreviewItem[] | null>(null)
const previewLoading = ref(false)

const progress = ref<ProgressPayload | null>(null)
const stats = ref<ExecuteStats | null>(null)
let unlistenProgress: (() => void) | null = null
let dragUnlisten: (() => void) | null = null
const dropActive = ref(false)

// 弹窗
const modalVisible = ref(false)
const modalRef = ref<HTMLElement | null>(null)
const overlayRef = ref<HTMLElement | null>(null)

// 首页动画目标
const dropzoneRef = ref<HTMLElement | null>(null)
const dropIconRef = ref<HTMLElement | null>(null)
const tipsRef = ref<HTMLElement | null>(null)
const configPanelRef = ref<HTMLElement | null>(null)

const prefersReducedMotion = () =>
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

/** 首页入场：拖拽区上浮淡入 + 提示逐条浮现 + 图标悬浮循环 */
function animateHomeEntrance() {
  if (prefersReducedMotion() || !dropzoneRef.value) return
  gsap.from(dropzoneRef.value, { autoAlpha: 0, y: 28, duration: 0.5, ease: 'power3.out' })
  if (tipsRef.value) {
    gsap.from(tipsRef.value.querySelectorAll('li'), {
      autoAlpha: 0,
      y: 12,
      duration: 0.4,
      ease: 'power2.out',
      stagger: 0.08,
      delay: 0.15,
    })
  }
  if (dropIconRef.value) {
    // 拖拽区图标缓慢悬浮，暗示"可投放"
    gsap.to(dropIconRef.value, {
      y: -7,
      duration: 1.6,
      ease: 'sine.inOut',
      repeat: -1,
      yoyo: true,
    })
  }
}

/** 文件导入后配置面板入场 */
function animateConfigEntrance() {
  if (prefersReducedMotion() || !configPanelRef.value) return
  gsap.from(configPanelRef.value, {
    autoAlpha: 0,
    y: 20,
    duration: 0.45,
    ease: 'power3.out',
  })
  const cards = configPanelRef.value.querySelectorAll('.mode-card')
  if (cards.length) {
    gsap.from(cards, {
      autoAlpha: 0,
      y: 14,
      duration: 0.35,
      ease: 'power2.out',
      stagger: 0.05,
      delay: 0.12,
    })
  }
}

watch(preview, (nv, ov) => {
  if (nv && !ov) {
    void nextTick(() => animateConfigEntrance())
  }
})

const modeOptions: { value: DecodeMode; title: string; desc: string }[] = [
  { value: 'auto', title: '自动识别（推荐）', desc: '自动判断每行密文类型' },
  { value: 'log', title: 'log 密文', desc: '含 Β / Α 分隔符，本地解密，无需网络' },
  { value: 'md5', title: 'md5 摘要', desc: '32 位十六进制，内网服务远程查表' },
  { value: 'sha256', title: 'sha256 摘要', desc: '64 位十六进制，内网服务远程查表' },
]

const allSuccess = computed(() => {
  if (!stats.value) return false
  const s = stats.value
  return (
    s.success + s.plaintext + s.empty === s.total_rows &&
    s.failed + s.not_found + s.grpc_error_rows + s.invalid_format === 0
  )
})

const stepIndex = computed(() => ({ import: 0, executing: 1, done: 2 })[step.value] ?? 0)

onMounted(async () => {
  animateHomeEntrance()
  unlistenProgress = await onProgress((p) => {
    progress.value = p
  })
  const win = getCurrentWebviewWindow()
  dragUnlisten = await win.onDragDropEvent((event) => {
    if (event.payload.type === 'over') {
      dropActive.value = true
    } else if (event.payload.type === 'leave') {
      dropActive.value = false
    } else if (event.payload.type === 'drop') {
      dropActive.value = false
      const paths = (event.payload as { paths: string[] }).paths
      if (paths && paths.length > 0) {
        void handleFile(paths[0])
      }
    }
  })
})

onUnmounted(() => {
  unlistenProgress?.()
  dragUnlisten?.()
})

async function pickFile() {
  const selected = await open({
    multiple: false,
    title: '选择要解密的文件',
    filters: [{ name: 'Excel / CSV', extensions: ['xlsx', 'csv'] }],
  })
  if (selected) {
    await handleFile(selected)
  }
}

async function handleFile(path: string) {
  error.value = ''
  preview.value = null
  previewItems.value = null
  filePath.value = path
  loading.value = '正在解析文件…'
  try {
    preview.value = await importFile(path)
    step.value = 'import'
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = ''
  }
}

async function runPreview() {
  if (!preview.value || !filePath.value) return
  previewLoading.value = true
  previewItems.value = null
  error.value = ''
  try {
    previewItems.value = await previewDecrypt(
      filePath.value,
      preview.value.sheet,
      0,
      mode.value,
      grpcConfig.value,
    )
  } catch (e) {
    error.value = String(e)
  } finally {
    previewLoading.value = false
  }
}

async function testPing() {
  pingState.value = { status: 'testing', message: '' }
  try {
    const pong = await pingGrpc(grpcConfig.value)
    pingState.value = { status: 'ok', message: `连通正常（${pong}）` }
  } catch (e) {
    pingState.value = { status: 'fail', message: String(e) }
  }
}

async function run() {
  if (!preview.value || !filePath.value) return
  step.value = 'executing'
  progress.value = { phase: 'reading', processed: 0, total: 0, message: '准备中…' }
  error.value = ''
  try {
    stats.value = await executeDecrypt({
      path: filePath.value,
      sheet: preview.value.sheet,
      column: 0,
      mode: mode.value,
      grpc: grpcConfig.value,
      output_column_name: outputColumnName.value,
    })
    step.value = 'done'
    showModal()
  } catch (e) {
    error.value = String(e)
    step.value = 'import'
  }
}

async function showModal() {
  modalVisible.value = true
  await nextTick()
  if (!modalRef.value || !overlayRef.value) return
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches
  if (reduceMotion) return
  gsap.from(overlayRef.value, { autoAlpha: 0, duration: 0.25, ease: 'power1.out' })
  gsap.from(modalRef.value, {
    scale: 0.82,
    autoAlpha: 0,
    y: 24,
    duration: 0.45,
    ease: 'back.out(1.6)',
  })
}

function closeModal() {
  // 关闭弹窗 → 直接回到启动页（避免关闭后白板）
  reset()
}

async function cancel() {
  await cancelDecrypt()
}

async function reveal(path: string) {
  await revealItemInDir(path)
}

function reset() {
  step.value = 'import'
  preview.value = null
  previewItems.value = null
  stats.value = null
  progress.value = null
  filePath.value = ''
  modalVisible.value = false
  error.value = ''
}

const progressPercent = computed(() => {
  const p = progress.value
  if (!p || p.total === 0) return 0
  return Math.min(100, Math.round((p.processed / p.total) * 100))
})

const phaseLabel: Record<string, string> = {
  reading: '读取文件',
  analyzing: '分析密文',
  decoding: '远程查表',
  writing: '写出文件',
  done: '完成',
}

function fmtInt(n: number): string {
  return n.toLocaleString()
}

function fmtDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  const s = ms / 1000
  if (s < 60) return `${s.toFixed(1)} 秒`
  const m = Math.floor(s / 60)
  return `${m} 分 ${Math.round(s % 60)} 秒`
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) + '…' : s
}
</script>

<template>
  <div class="app">
    <header class="header">
      <div class="header-title">
        <h1>解码宝匣 <span class="header-en">DecodeBox</span></h1>
        <p>批量解密加密手机号，追加解密列 · 支持 log / md5 / sha256</p>
      </div>
      <div class="steps">
        <div
          v-for="(s, i) in ['导入文件', '执行', '完成']"
          :key="s"
          class="step"
          :class="{ active: stepIndex >= i, current: stepIndex === i }"
        >
          <span class="step-num">{{ i + 1 }}</span>
          <span class="step-name">{{ s }}</span>
        </div>
      </div>
    </header>

    <main class="content">
      <div v-if="error" class="alert error">{{ error }}</div>
      <div v-if="loading" class="alert info">{{ loading }}</div>

      <!-- 第 1 步：导入 + 选择解密方式 -->
      <section v-if="step === 'import'" class="panel">
        <template v-if="!preview">
          <div
            ref="dropzoneRef"
            class="dropzone"
            :class="{ active: dropActive }"
            @click="pickFile"
          >
            <div ref="dropIconRef" class="dropzone-icon">📁</div>
            <div class="dropzone-title">拖拽文件到此处，或点击选择</div>
            <div class="dropzone-sub">支持 .xlsx / .csv</div>
          </div>
          <ul ref="tipsRef" class="tips">
            <li>log 密文（含希腊字母 Β / Α）：本地解密，<strong>离线可用</strong></li>
            <li>
              md5（32 位）/ sha256（64 位）：单向哈希，<strong>需开启 VPN 连接内网加解密服务</strong>才能解密
            </li>
            <li>输出为新文件（原名_decrypted），原文件绝不修改</li>
          </ul>
        </template>

        <template v-else>
          <div ref="configPanelRef" class="config-panel">
          <div class="file-summary">
            <span class="file-name">📄 {{ preview.file_name }}</span>
            <span class="chip">{{ preview.file_type.toUpperCase() }}</span>
            <span class="chip">共 {{ fmtInt(preview.total_rows) }} 行数据</span>
            <button class="btn secondary small" @click="reset">更换文件</button>
          </div>

          <h3>解密方式</h3>
          <div class="mode-grid">
            <div
              v-for="m in modeOptions"
              :key="m.value"
              class="mode-card"
              :class="{ selected: mode === m.value }"
              @click="mode = m.value"
            >
              <div class="mode-title">{{ m.title }}</div>
              <div class="mode-desc">{{ m.desc }}</div>
            </div>
          </div>
          <div v-if="mode !== 'log'" class="alert warn">
            ⚠ {{ mode === 'md5' ? 'md5' : mode === 'sha256' ? 'sha256' : 'md5 / sha256' }}
            解密需要连接内网加解密服务，请确认已开启 VPN，否则这些行将解密失败
          </div>

          <div class="sample-table-wrap">
            <table class="data-table">
              <thead>
                <tr>
                  <th v-for="(h, i) in preview.headers" :key="i">{{ h || '(空表头)' }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="(row, r) in preview.sample_rows" :key="r">
                  <td v-for="(cell, c) in row" :key="c" :title="cell">{{ truncate(cell, 60) }}</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div class="config-row">
            <label class="field">
              <span>解密结果列名</span>
              <input v-model="outputColumnName" type="text" maxlength="30" />
            </label>
            <div class="field actions">
              <button class="btn secondary" :disabled="previewLoading" @click="runPreview">
                {{ previewLoading ? '试解中…' : '试解前 5 条' }}
              </button>
              <button class="btn primary" @click="run">开始解密</button>
            </div>
          </div>

          <div v-if="previewItems" class="preview-result">
            <h4>试解结果</h4>
            <table class="data-table small">
              <thead>
                <tr>
                  <th>行号</th>
                  <th>类型</th>
                  <th>原值</th>
                  <th>解密结果</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="it in previewItems" :key="it.row_no">
                  <td>{{ it.row_no }}</td>
                  <td>{{ it.kind }}</td>
                  <td class="mono" :title="it.original">{{ truncate(it.original, 36) }}</td>
                  <td class="mono" :class="{ ok: it.ok, bad: !it.ok }">{{ it.result || '(空)' }}</td>
                </tr>
              </tbody>
            </table>
            <p v-if="previewItems.some((i) => !i.ok)" class="warn-note">
              ⚠ 存在解密失败 / 查无映射的值，正式执行时会记入失败清单
            </p>
          </div>

          <details class="advanced">
            <summary>高级设置（远程服务连接，默认已内置，无需修改）</summary>
            <div class="advanced-body">
              <label class="field">
                <span>服务地址</span>
                <input v-model="grpcConfig.target" type="text" />
              </label>
              <label class="field">
                <span>appName</span>
                <input v-model="grpcConfig.app_name" type="text" />
              </label>
              <label class="field">
                <span>appSecretKey</span>
                <input v-model="grpcConfig.app_secret" type="password" />
              </label>
              <label class="field narrow">
                <span>并发数</span>
                <input v-model.number="grpcConfig.concurrency" type="number" min="1" max="500" />
              </label>
              <label class="field narrow">
                <span>超时(ms)</span>
                <input v-model.number="grpcConfig.timeout_ms" type="number" min="1000" max="60000" />
              </label>
              <div class="field actions">
                <button class="btn ghost" :disabled="pingState.status === 'testing'" @click="testPing">
                  {{ pingState.status === 'testing' ? '测试中…' : '测试连接' }}
                </button>
                <span v-if="pingState.status === 'ok'" class="ping ok">✓ {{ pingState.message }}</span>
                <span v-if="pingState.status === 'fail'" class="ping fail">✗ {{ pingState.message }}</span>
              </div>
            </div>
          </details>
          </div>
        </template>
      </section>

      <!-- 第 2 步：执行 -->
      <section v-if="step === 'executing'" class="panel executing">
        <div class="phase">{{ phaseLabel[progress?.phase ?? 'reading'] ?? '处理中' }}</div>
        <div class="progress-message">{{ progress?.message ?? '准备中…' }}</div>
        <div class="progress-bar">
          <div class="progress-fill" :style="{ width: progressPercent + '%' }"></div>
        </div>
        <div class="progress-numbers">
          <span v-if="progress && progress.total > 0">
            {{ fmtInt(progress.processed) }} / {{ fmtInt(progress.total) }}（{{ progressPercent }}%）
          </span>
          <span v-else>正在处理…</span>
        </div>
        <button class="btn danger" @click="cancel">取消</button>
      </section>

      <!-- 完成弹窗（右上角 ✕ 关闭并回到首页） -->
      <div v-if="modalVisible && stats" class="modal-overlay" ref="overlayRef" @click.self="closeModal">
        <div class="modal" ref="modalRef">
          <button class="modal-close" title="关闭并返回首页" @click="closeModal">✕</button>
          <template v-if="stats.cancelled">
            <div class="modal-icon warn">⚠</div>
            <div class="modal-title">已取消</div>
            <p class="modal-desc">未生成输出文件。</p>
          </template>
          <template v-else-if="allSuccess">
            <div class="modal-icon ok">✓</div>
            <div class="modal-title">解密完成</div>
            <p class="modal-desc">共 {{ fmtInt(stats.total_rows) }} 行，全部处理完成</p>
            <div class="modal-stat-row">
              <div class="modal-stat good">
                <div class="ms-num">{{ fmtInt(stats.success) }}</div>
                <div class="ms-label">解密成功</div>
              </div>
              <div v-if="stats.plaintext > 0" class="modal-stat">
                <div class="ms-num">{{ fmtInt(stats.plaintext) }}</div>
                <div class="ms-label">明文透传</div>
              </div>
              <div v-if="stats.empty > 0" class="modal-stat">
                <div class="ms-num">{{ fmtInt(stats.empty) }}</div>
                <div class="ms-label">空值</div>
              </div>
              <div class="modal-stat">
                <div class="ms-num">{{ fmtDuration(stats.duration_ms) }}</div>
                <div class="ms-label">耗时</div>
              </div>
            </div>
            <p v-if="stats.plaintext > 0" class="modal-desc">
              「明文透传」= 原文件中本就是明文手机号的行，未做解密、原样保留
            </p>
          </template>
          <template v-else>
            <div class="modal-icon warn">⚠</div>
            <div class="modal-title">解密完成（部分失败）</div>
            <p class="modal-desc">
              成功 {{ fmtInt(stats.success) }} 行，失败
              {{ fmtInt(stats.failed + stats.not_found + stats.grpc_error_rows + stats.invalid_format) }}
              行。失败数据已单独导出。
            </p>
            <div class="modal-stat-row">
              <div class="modal-stat good">
                <div class="ms-num">{{ fmtInt(stats.success) }}</div>
                <div class="ms-label">成功</div>
              </div>
              <div class="modal-stat bad">
                <div class="ms-num">
                  {{ fmtInt(stats.failed + stats.not_found + stats.grpc_error_rows + stats.invalid_format) }}
                </div>
                <div class="ms-label">失败</div>
              </div>
            </div>
          </template>

          <div v-if="!stats.cancelled" class="modal-output">
            <div class="modal-output-row">
              <span class="mo-label">输出文件</span>
              <code class="mo-path">{{ stats.output_path }}</code>
            </div>
            <div v-if="stats.failures_path" class="modal-output-row">
              <span class="mo-label">失败数据</span>
              <code class="mo-path">{{ stats.failures_path }}</code>
            </div>
          </div>

          <div class="modal-actions">
            <button v-if="!stats.cancelled" class="btn primary" @click="reveal(stats.output_path)">
              打开所在目录
            </button>
            <button class="btn ghost" @click="reset">再处理一个</button>
          </div>
        </div>
      </div>
    </main>

    <footer class="footer">
      解密结果含明文手机号，属敏感数据，请妥善保管 · 原文件不会被修改
    </footer>
  </div>
</template>

<style>
@import './style.css';
</style>

