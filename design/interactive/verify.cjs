// Browser-level acceptance checks for the design artifact; no production API is called.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.INPUTSWITCH_PLAYWRIGHT || 'playwright');

const capture = process.argv.includes('--capture');
const captures = path.resolve(__dirname, '../../.impeccable/review');
const base = process.env.INPUTSWITCH_PREVIEW_URL || pathToFileURL(path.join(__dirname, 'index.html')).href;
const reports = [];
const layout = [];
const errors = [];
const forbidden = [];
let browser;
let context;
let page;

function url(parameters = {}) {
  const target = new URL(base);
  Object.entries(parameters).forEach(([key, value]) => target.searchParams.set(key, value));
  return target.href;
}
async function fresh(parameters = {}, viewport = { width: 1080, height: 720 }) {
  if (context) await context.close();
  context = await browser.newContext({ viewport, reducedMotion: 'reduce' });
  page = await context.newPage();
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => { if (/\/api\//.test(request.url())) forbidden.push(request.url()); });
  await page.goto(url({ embed: '1', ...parameters }));
  await page.waitForTimeout(100);
}
async function test(name, run) {
  try { await run(); reports.push({ name, passed: true }); }
  catch (error) { reports.push({ name, passed: false, error: error.message }); }
}
async function bounds(label) {
  const result = await page.evaluate(() => {
    const app = document.querySelector('#appWindow');
    const view = document.querySelector('#viewContainer');
    const box = app.getBoundingClientRect();
    const controls = Array.from(document.querySelectorAll('#mainSwitch,.maintenance-actions button,.page-view:not([hidden]) button'));
    const outside = controls.filter(element => {
      const rect = element.getBoundingClientRect();
      return rect.width && rect.height && (rect.left < box.left - 1 || rect.right > box.right + 1 || rect.bottom > box.bottom - 29 || rect.top < box.top);
    }).map(element => element.getAttribute('aria-label') || element.textContent.trim());
    return { width: app.clientWidth, height: app.clientHeight, overflowY: view.scrollHeight - view.clientHeight, overflowX: document.documentElement.scrollWidth - innerWidth, outside };
  });
  layout.push({ label, ...result });
}
async function shot(name) { if (capture) await page.screenshot({ path: path.join(captures, name + '.png'), fullPage: true, animations: 'disabled' }); }

(async () => {
  fs.mkdirSync(captures, { recursive: true });
  browser = await chromium.launch({ channel: process.platform === 'win32' ? 'msedge' : undefined, headless: true });
  await test('切换等待确认后才同时更新画面与键鼠归属', async () => {
    await fresh();
    await page.locator('#mainSwitch').click();
    assert.equal(await page.locator('#pictureOwner').innerText(), '本机');
    assert.equal(await page.locator('#mainSwitch').getAttribute('aria-busy'), 'true');
    assert.equal(await page.locator('#mainSwitch').isDisabled(), false);
    await page.waitForFunction(() => document.querySelector('#pictureOwner').textContent === 'Mac mini');
    assert.equal(await page.locator('#keyboardOwner').innerText(), 'Mac mini');
    await page.keyboard.press('Control+Alt+Escape');
    assert.equal(await page.locator('#keyboardOwner').innerText(), '本机');
  });
  await test('单屏与多屏视图切换不修改设备布局', async () => {
    await fresh();
    await page.locator('[data-scene="multi"]').click();
    assert.equal(await page.locator('.slot-right[data-peer="think"]').count(), 1);
    await page.locator('[data-scene="single"]').click();
    await page.locator('[data-scene="multi"]').click();
    assert.equal(await page.locator('.slot-right[data-peer="think"]').count(), 1);
    await page.locator('.multi-explainer [data-action="cross-screen"]').click();
    assert.equal(await page.locator('#pictureOwner').innerText(), '本机');
    assert.equal(await page.locator('#keyboardOwner').innerText(), 'ThinkPad');
  });
  await test('等待切屏期间紧急断开，迟到确认不能再次交走键鼠', async () => {
    await fresh();
    await page.locator('#mainSwitch').click();
    await page.keyboard.press('Control+Alt+Escape');
    await page.waitForFunction(() => document.querySelector('#pictureOwner').textContent === 'Mac mini');
    assert.equal(await page.locator('#keyboardOwner').innerText(), '本机');
  });
  await test('一体化壳体占满窗口，设计工具通过独立入口展开', async () => {
    await fresh({ embed: '0' });
    const box = await page.locator('#appWindow').boundingBox();
    assert.deepEqual({ x: box.x, y: box.y, width: box.width, height: box.height }, { x: 0, y: 0, width: 1080, height: 720 });
    assert.equal(await page.locator('#previewTools').isVisible(), false);
    await page.locator('[popovertarget="previewTools"]').click();
    assert.equal(await page.locator('#previewTools').isVisible(), true);
    await page.locator('#demoState').selectOption('offline');
    assert.match(await page.locator('#mainSwitch').innerText(), /离线/);
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('#previewTools').isVisible(), false);
  });
  await test('场景与设置切换后保留触发控件的键盘焦点', async () => {
    await fresh();
    await page.locator('[data-scene="multi"]').click();
    assert.equal(await page.evaluate(() => document.activeElement.dataset.scene), 'multi');
    await page.locator('.main-nav [data-page="settings"]').click();
    await page.locator('[data-settings-tab="startup"]').click();
    assert.equal(await page.evaluate(() => document.activeElement.dataset.settingsTab), 'startup');
  });
  await test('弹窗及配对输入中均可紧急断开，不丢失草稿', async () => {
    await fresh({ scene: 'multi' });
    await page.locator('.multi-explainer [data-action="cross-screen"]').click();
    await page.locator('.maintenance-actions [data-maintain="scan"]').click();
    await page.keyboard.press('Control+Alt+Escape');
    assert.equal(await page.locator('#keyboardOwner').innerText(), '本机');
    assert.match(await page.locator('#emergencyNotice').innerText(), /键鼠已回到本机/);
    await page.locator('#modal [aria-label="关闭对话框"]').click();
    await page.locator('.multi-explainer [data-action="cross-screen"]').click();
    await page.locator('.main-nav [data-page="devices"]').click();
    await page.locator('#devicesView .view-heading [data-action="pair"]').click();
    await page.locator('#pairCode').fill('draft-code');
    await page.keyboard.press('Control+Alt+Escape');
    assert.equal(await page.locator('#keyboardOwner').innerText(), '本机');
    assert.equal(await page.locator('#pairCode').inputValue(), 'draft-code');
    assert.equal(await page.evaluate(() => document.activeElement.id), 'pairCode');
  });
  await test('维护请求的等待提示不使用成功图标', async () => {
    await fresh();
    await page.locator('.maintenance-actions [data-maintain="detect"]').click();
    assert.equal(await page.locator('.toast').getAttribute('data-kind'), 'pending');
    assert.equal(await page.locator('.toast .busy-icon').count(), 1);
    await page.waitForFunction(() => document.querySelector('.toast')?.dataset.kind === 'success');
  });
  await test('传输进度更新时不移走暂停按钮焦点', async () => {
    await fresh({ page: 'transfers' });
    await page.locator('[data-action="demo-transfer"]').click();
    await page.locator('.transfer-row').first().locator('[data-transfer-action="pause"]').focus();
    await page.waitForTimeout(800);
    assert.equal(await page.evaluate(() => document.activeElement.dataset.transferAction), 'pause');
  });
  await test('浅色成功标签的文字对比度至少 4.5 比 1', async () => {
    await fresh();
    const contrast = await page.locator('.scene-panel .tag.good').evaluate(element => {
      const style = getComputedStyle(element);
      const luminance = value => value.match(/\d+/g).slice(0, 3).map(Number).map(channel => channel / 255).map(channel => channel <= 0.04045 ? channel / 12.92 : Math.pow((channel + 0.055) / 1.055, 2.4)).reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
      const foreground = luminance(style.color), background = luminance(style.backgroundColor);
      return (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05);
    });
    assert.ok(contrast >= 4.5, 'contrast: ' + contrast);
  });
  await test('共享开关跨页面同步，并在刷新后保留', async () => {
    await fresh();
    await page.locator('[data-setting="clipboard"]').click();
    await page.locator('.main-nav [data-page="settings"]').click();
    assert.equal(await page.locator('#settingsView [data-setting="clipboard"]').getAttribute('aria-checked'), 'false');
    await page.reload();
    assert.equal(await page.locator('[data-setting="clipboard"]').getAttribute('aria-checked'), 'false');
  });
  await test('离线时主动作仍可点，并提供可恢复的解释', async () => {
    await fresh({ state: 'offline' });
    assert.equal(await page.locator('#mainSwitch').isVisible(), true);
    assert.equal(await page.locator('#mainSwitch').isDisabled(), false);
    await page.locator('#mainSwitch').click();
    assert.match(await page.locator('#toastStack').innerText(), /离线/);
    assert.equal(await page.locator('#pictureOwner').innerText(), '本机');
  });
  await test('失败状态不会谎报已切换', async () => {
    await fresh({ state: 'failure' });
    await page.locator('#mainSwitch').click();
    await page.waitForFunction(() => document.querySelector('#mainSwitch').getAttribute('aria-busy') === 'false');
    assert.equal(await page.locator('#pictureOwner').innerText(), '本机');
    assert.match(await page.locator('#toastStack').innerText(), /未完成/);
  });
  await test('配对校验、等待与确认三种反馈完整', async () => {
    await fresh({ state: 'unpaired' });
    await page.locator('#mainSwitch').click();
    await page.locator('#pairCode').fill('');
    await page.locator('#pairForm [type="submit"]').click();
    assert.match(await page.locator('#pairHelp').innerText(), /请填写/);
    await page.locator('#pairCode').fill('design-738291');
    await page.locator('#pairForm [type="submit"]').click();
    assert.match(await page.locator('#pairHelp').innerText(), /等待/);
    assert.match(await page.locator('#footerConnection').innerText(), /尚未配对/);
    await page.waitForFunction(() => document.querySelector('#footerConnection').textContent.includes('连接已加密'));
  });
  await test('设备支持点放、撤销、方向键与拖动', async () => {
    await fresh({ page: 'devices' });
    await page.locator('.device-row[data-peer="think"]').click();
    await page.locator('.slot-up').click();
    assert.equal(await page.locator('.slot-up[data-peer="think"]').count(), 1);
    await page.locator('[data-action="undo"]').click();
    assert.equal(await page.locator('.slot-right[data-peer="think"]').count(), 1);
    await page.keyboard.press('ArrowLeft');
    assert.equal(await page.locator('.slot-left[data-peer="think"]').count(), 1);
    await page.locator('.slot-left[data-peer="think"]').dragTo(page.locator('.slot-down'));
    assert.equal(await page.locator('.slot-down[data-peer="think"]').count(), 1);
  });
  await test('文件传输可暂停、继续、取消并重试完成', async () => {
    await fresh({ page: 'transfers' });
    await page.locator('[data-action="demo-transfer"]').click();
    await page.waitForTimeout(420);
    await page.locator('.transfer-row').first().locator('[data-transfer-action="pause"]').click();
    const paused = await page.locator('.transfer-row').first().locator('[role="progressbar"]').getAttribute('aria-valuenow');
    await page.waitForTimeout(450);
    assert.equal(await page.locator('.transfer-row').first().locator('[role="progressbar"]').getAttribute('aria-valuenow'), paused);
    await page.locator('.transfer-row').first().locator('[data-transfer-action="pause"]').click();
    await page.locator('.transfer-row').first().locator('[data-transfer-action="cancel"]').click();
    assert.match(await page.locator('.transfer-row').first().innerText(), /已取消/);
    await page.locator('.transfer-row').first().locator('[data-transfer-action="retry"]').click();
    await page.waitForFunction(() => document.querySelector('.transfer-row').textContent.includes('已完成'), { timeout: 7000 });
  });
  await test('扫描需要说明，取消与 Esc 可恢复原焦点', async () => {
    await fresh();
    const trigger = page.locator('.maintenance-actions [data-maintain="scan"]');
    await trigger.click();
    assert.equal(await page.locator('#modal').isVisible(), true);
    assert.match(await page.locator('#modal').innerText(), /短暂变黑/);
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('#modal').isVisible(), false);
    assert.equal(await trigger.evaluate(element => document.activeElement === element), true);
    assert.equal(await trigger.isDisabled(), false);
  });
  await test('快捷键录制、保存与真实按键触发', async () => {
    await fresh({ page: 'settings' });
    await page.locator('[data-action="record-hotkey"]').click();
    await page.keyboard.press('Control+Shift+K');
    assert.match(await page.locator('.keybind').innerText(), /Ctrl/);
    await page.keyboard.press('Control+Shift+K');
    await page.waitForFunction(() => document.querySelector('#pictureOwner').textContent === 'Mac mini');
  });
  await test('无输入口的主按钮保持可见，且不存在可用的 HDMI 操作', async () => {
    await fresh({ state: 'noinput' });
    assert.equal(await page.locator('#mainSwitch').isDisabled(), false);
    await page.locator('#mainSwitch').click();
    assert.match(await page.locator('#toastStack').innerText(), /没有其他可用输入口/);
    assert.equal(await page.getByRole('button', { name: 'HDMI · 无信号' }).isDisabled(), true);
  });
  await test('文件名和输入源名称作为文本渲染', async () => {
    await fresh();
    await page.locator('[data-action="ports"]').click();
    await page.locator('#port-mac').fill('<img src=x onerror=1>');
    await page.locator('[data-action="save-ports"]').click();
    assert.equal(await page.locator('img[onerror]').count(), 0);
    assert.match(await page.locator('.source-strip').innerText(), /<img/);
  });

  for (const viewport of [{ width: 1080, height: 720 }, { width: 820, height: 680 }, { width: 600, height: 520 }, { width: 390, height: 844 }]) {
    for (const pageName of ['workspace', 'devices', 'transfers', 'settings']) {
      await fresh({ page: pageName }, viewport);
      await bounds(viewport.width + '-' + pageName);
      if (viewport.width === 1080 || viewport.width === 600 || (viewport.width === 390 && pageName === 'workspace')) await shot(viewport.width + '-' + pageName);
    }
    await fresh({ scene: 'multi' }, viewport);
    await bounds(viewport.width + '-multi');
    if (viewport.width !== 820) await shot(viewport.width + '-multi');
    await fresh({ state: 'offline' }, viewport);
    await bounds(viewport.width + '-offline');
    if (viewport.width !== 390) {
      await fresh({ page: 'devices' }, viewport);
      await page.locator('#devicesView .view-heading [data-action="pair"]').click();
      await bounds(viewport.width + '-pairing');
      await fresh({ page: 'settings' }, viewport);
      for (const tab of ['startup', 'appearance']) {
        await page.locator('[data-settings-tab="' + tab + '"]').click();
        await bounds(viewport.width + '-settings-' + tab);
      }
    }
  }
  await fresh({ theme: 'dark' }, { width: 1080, height: 720 });
  await bounds('dark-workspace');
  await shot('dark-workspace');
  await fresh({}, { width: 1440, height: 930 });
  await page.goto(url());
  await page.waitForTimeout(120);
  await shot('desktop');
  await page.locator('[popovertarget="previewTools"]').click();
  await shot('preview-tools');
  if (capture) fs.writeFileSync(path.join(captures, 'acceptance.json'), JSON.stringify({ reports, layout, errors, forbidden }, null, 2));
  const failedLayouts = layout.filter(result => result.overflowY > 1 || result.overflowX > 1 || result.outside.length);
  console.log(JSON.stringify({ tests: reports, failedLayouts, javascriptErrors: errors, productionRequests: forbidden, captures: capture ? captures : undefined }, null, 2));
  await browser.close();
  if (reports.some(result => !result.passed) || errors.length || forbidden.length || failedLayouts.length) process.exitCode = 1;
})().catch(async error => { console.error(error); if (browser) await browser.close(); process.exitCode = 1; });
