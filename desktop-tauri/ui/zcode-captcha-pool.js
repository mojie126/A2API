/* 套餐池守卫概览；实际模型验证始终绑定用户点选的账号和配置。
 * 自动补货：上游要求验证码而库存为空（needsTokens）时，轮询循环自动对绑定
 * 账号执行一次无痕验证并入池，不再依赖人工点「开始验证」；失败按指数退避，
 * 成功或库存恢复即停。免码期（required=false）不铸，避免烧风控配额。 */
(() => {
  const POLL_MS = 2500;
  const IDLE_POLL_MS = 15000;
  /** 自动补货失败后的退避起点与封顶（成功清零；封顶 10 分钟与网关闸门 TTL 同量级） */
  const REFILL_BACKOFF_START_MS = 30_000;
  const REFILL_BACKOFF_MAX_MS = 10 * 60_000;
  /** 两次自动铸入之间的最小间隔：令牌寿命 120 秒，跟上消费节奏即可，不抢节奏 */
  const REFILL_MIN_GAP_MS = 60_000;
  let timer = 0;
  let stopped = false;
  let running = false;
  let generation = 0;
  let active = null;
  let stats = null;
  let status = { state: 'idle', error: '', accountId: '', provider: '' };
  const accountStats = new Map();
  const accountStatuses = new Map();
  const accountReads = new Map();
  let readSequence = 0;
  /** 自动补货的退避与节奏状态（手动「开始验证」不受它们约束） */
  let refillNextAt = 0;
  let refillBackoff = 0;
  let refillLastOkAt = 0;

  const idle = accountId => ({ state: 'idle', error: '', accountId, provider: '' });
  const providerValid = value => value === 'zcode' || value === 'zcode-intl';
  const text = value => typeof value === 'string' && value.trim() && !/[\u0000-\u001f\u007f]/.test(value);
  const cancelled = () => new DOMException('请求已取消', 'AbortError');
  function remember(map, key, value) {
    if (!map.has(key) && map.size >= 128) map.delete(map.keys().next().value);
    map.set(key, value);
  }

  function publish(state, error = '', accountId = '', provider = '') {
    const next = { state, error: String(error || ''), accountId, provider };
    const previous = accountId ? accountStatuses.get(accountId) : status;
    if (previous && previous.state === next.state && previous.error === next.error && previous.provider === provider) return;
    if (accountId) remember(accountStatuses, accountId, next);
    else status = next;
    window.dispatchEvent(new CustomEvent('zcode-captcha-status', { detail: { ...next } }));
  }

  function bindingOf(config, accountId) {
    if (!config || typeof config !== 'object' || Array.isArray(config) || typeof config.enabled !== 'boolean') throw new Error('invalid_captcha_config');
    if (config.enabled && (!text(config.region) || !text(config.prefix) || !text(config.sceneId))) throw new Error('invalid_captcha_config');
    const bound = ['provider', 'accountId', 'configId', 'engine'].some(key => Object.prototype.hasOwnProperty.call(config, key));
    if (!bound) return null;
    if (!providerValid(config.provider) || config.accountId !== accountId || !text(config.configId)
      || (config.enabled ? config.engine !== 'aliyun' : !['none', 'aliyun'].includes(config.engine))) throw new Error('invalid_captcha_binding');
    return { provider: config.provider, accountId, configId: config.configId };
  }

  function matchingStats(value, accountId, provider) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
    if (!accountId) return true;
    const bound = ['provider', 'accountId'].some(key => Object.prototype.hasOwnProperty.call(value, key));
    if (bound || provider) return providerValid(value.provider) && value.accountId === accountId && value.captchaAccountId === accountId
      && (!provider || value.provider === provider);
    return value.captchaAccountId === undefined || value.captchaAccountId === null || value.captchaAccountId === accountId;
  }

  function schedule(delay) {
    window.clearTimeout(timer);
    if (!stopped) timer = window.setTimeout(() => void check(), delay);
  }

  async function readStats(accountId = '', provider, signal) {
    const bridge = window.workbuddyDesktop;
    const epoch = generation;
    const origin = window.location.origin;
    const requestId = ++readSequence;
    if (accountId) remember(accountReads, accountId, requestId);
    const requireCurrent = () => {
      if (signal?.aborted || epoch !== generation || bridge !== window.workbuddyDesktop || origin !== window.location.origin) throw cancelled();
    };
    try {
      requireCurrent();
      if (!bridge?.zcodeCaptchaStats || (provider !== undefined && !providerValid(provider))) throw new Error('captcha_api_unavailable');
      const value = await bridge.zcodeCaptchaStats(accountId || undefined, provider);
      requireCurrent();
      if (!matchingStats(value, accountId, provider)) throw new Error('invalid_captcha_scope');
      if (value.error || value.configError) throw new Error('captcha_status_unavailable');
      if (accountId) {
        if (accountReads.get(accountId) === requestId) remember(accountStats, accountId, value);
      } else stats = value;
      return value;
    } catch (error) {
      requireCurrent();
      if (error?.name === 'AbortError') throw error;
      throw new Error(wbI18n.t('验证码状态读取失败，请刷新后重试'));
    }
  }

  function stateOf(value, accountId = '', provider = '') {
    if (value.error || value.configError) return publish('error', wbI18n.t('验证码状态读取失败，请刷新后重试'), accountId, provider);
    if (value.required === false) return publish('disabled', '', accountId, provider);
    if (Number(value.startPlanAccounts) > 0 && value.needsTokens === true) return publish('needs_verification', '', accountId, provider);
    publish('idle', '', accountId, provider);
  }

  /**
   * 自动补货：上游要求验证码而库存为空时，对绑定账号铸一枚入池。
   *
   * 触发条件（都来自无参读数）：required !== false（上游此刻真要码，免码期
   * 白铸烧风控配额）、needsTokens === true（库存满足不了需求）、读数带着
   * 绑定账号（accountId 是网关按 Start Plan 账号选定的铸造对象）。
   * 节奏：verify 自身单飞（手动按钮共存）；失败指数退避 30 秒起步、封顶
   * 10 分钟；两次成功铸入至少隔 60 秒；库存恢复（needsTokens 变 false）
   * 自动停，退避状态清零。
   *
   * @param value 无参读数（check 轮询取得）
   */
  async function autoRefill(value) {
    if (value.required === false || value.needsTokens !== true) {
      // 库存恢复或上游免码：清退避，下一轮回到常规节奏
      refillBackoff = 0;
      refillNextAt = 0;
      return;
    }
    const accountId = text(value.accountId) ? value.accountId : '';
    if (!accountId || active || Date.now() < refillNextAt
      || Date.now() - refillLastOkAt < REFILL_MIN_GAP_MS) return;
    const at = Date.now();
    try {
      const minted = await verify(accountId);
      if (minted === true) {
        refillLastOkAt = at;
        refillBackoff = 0;
        refillNextAt = 0;
      } else {
        // verify 返回 false = 读了配置后发现上游已转免码：小退避后复探
        refillNextAt = at + REFILL_BACKOFF_START_MS;
      }
    } catch {
      // 失败（网络 / 组件 / 提交未确认）：指数退避，把错误显示留给状态发布
      refillBackoff = refillBackoff > 0
        ? Math.min(REFILL_BACKOFF_MAX_MS, refillBackoff * 2)
        : REFILL_BACKOFF_START_MS;
      refillNextAt = at + refillBackoff;
    }
  }

  async function check() {
    if (running || stopped) return;
    const onAccounts = (window.wbApp?.currentPage || '') === 'accounts';
    // 自动补货不限于账号页：页面挂在任何一页都要能补（headless 部署没有
    // 人工点按钮的链路）；非账号页用低频节拍，账号页维持原有密集轮询。
    running = true;
    const epoch = generation;
    try {
      const value = await readStats();
      if (epoch !== generation || stopped) return;
      if (onAccounts) stateOf(value);
      await autoRefill(value);
      const poll = Number(value.pollMs) || POLL_MS;
      const delay = !onAccounts ? IDLE_POLL_MS
        : value.demandActive === true ? Math.min(30000, Math.max(1000, poll)) : IDLE_POLL_MS;
      schedule(delay);
    } catch {
      if (epoch === generation && !stopped) {
        if (onAccounts) publish('error', wbI18n.t('验证码状态读取失败，请刷新后重试'));
        schedule(IDLE_POLL_MS);
      }
    } finally {
      running = false;
    }
  }

  async function verify(accountId, signal, onPhase) {
    if (active) throw new Error(wbI18n.t('已有验证码流程正在进行，请先完成或取消'));
    const bridge = window.workbuddyDesktop;
    if (!text(accountId) || !bridge?.zcodeClaimCaptchaConfig || !bridge?.pushZcodeCaptchaTokens) {
      throw new Error(wbI18n.t('验证码令牌入池接口不可用'));
    }
    const current = { controller: new AbortController(), bridge, origin: window.location.origin, epoch: generation };
    const onAbort = () => current.controller.abort();
    signal?.addEventListener('abort', onAbort, { once: true });
    if (signal?.aborted) onAbort();
    active = current;
    let binding = null;
    let submitting = false;
    let accepted = false;
    const requireCurrent = () => {
      if (active !== current || current.controller.signal.aborted || current.epoch !== generation
        || current.bridge !== window.workbuddyDesktop || current.origin !== window.location.origin) throw cancelled();
    };
    const reportPhase = phase => { requireCurrent(); onPhase?.(phase); };
    publish('verifying', '', accountId);
    try {
      requireCurrent();
      reportPhase('preparing');
      const config = Object.freeze({ ...await bridge.zcodeClaimCaptchaConfig(accountId) });
      requireCurrent();
      binding = bindingOf(config, accountId);
      const value = await readStats(accountId, binding?.provider, current.controller.signal);
      requireCurrent();
      if (value.required === false) {
        stateOf(value, accountId, binding?.provider || '');
        return false;
      }
      if (!config.enabled) throw new Error('captcha_config_unavailable');
      const captcha = window.wbAliyunCaptcha;
      if (!captcha?.requestToken) throw new Error('captcha_component_unavailable');
      const token = await captcha.requestToken(config, current.controller.signal, { onPhase: reportPhase });
      requireCurrent();
      if (!text(token?.param)) throw new Error('invalid_captcha_token');
      reportPhase('submitting');
      submitting = true;
      const outcome = await bridge.pushZcodeCaptchaTokens(binding
        ? { ...binding, tokens: [{ param: token.param }] }
        : [{ param: token.param, region: config.region }]);
      requireCurrent();
      if (outcome?.accepted !== 1 || binding && (outcome.provider !== binding.provider || outcome.accountId !== accountId)) {
        throw new Error('captcha_submit_unconfirmed');
      }
      accepted = true;
      // POST 只是回执，不写入任何库存；回读同一个账号后才更新该账号状态。
      const refreshed = await readStats(accountId, binding?.provider, current.controller.signal);
      requireCurrent();
      stateOf(refreshed, accountId, binding?.provider || '');
      return true;
    } catch (error) {
      const stale = current.epoch !== generation || current.bridge !== window.workbuddyDesktop || current.origin !== window.location.origin;
      const wasCancelled = stale || current.controller.signal.aborted || ['AbortError', 'CaptchaCancelledError'].includes(error?.name);
      const message = wasCancelled ? '' : accepted ? wbI18n.t('验证码已提交，但状态刷新失败，请手动刷新')
        : submitting ? wbI18n.t('验证码提交结果未确认，请刷新状态后重新验证；不会自动重发')
          : wbI18n.t('验证码验证未完成，请刷新配置后重试');
      if (!stale) publish(wasCancelled ? 'paused' : 'error', message, accountId, binding?.provider || '');
      throw wasCancelled ? cancelled() : new Error(message);
    } finally {
      signal?.removeEventListener('abort', onAbort);
      if (active === current) active = null;
    }
  }

  function start() {
    stopped = false;
    schedule(1500);
  }

  function stop() {
    stopped = true;
    generation++;
    active?.controller.abort();
    accountStats.clear();
    accountStatuses.clear();
    accountReads.clear();
    stats = null;
    status = idle('');
    window.clearTimeout(timer);
  }

  window.wbZcodeCaptchaPool = {
    start, stop, verify, refresh: readStats,
    status: accountId => ({ ...(accountId ? accountStatuses.get(accountId) || idle(accountId) : status) }),
    stats: accountId => accountId ? accountStats.get(accountId) || null : stats,
  };
  window.addEventListener('wb:page', event => {
    if (event.detail === 'accounts' && !stopped) schedule(0);
  });
  window.addEventListener('pagehide', stop);
  window.addEventListener('pageshow', start);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start, { once: true });
  else start();
})();
