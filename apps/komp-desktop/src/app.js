const API = "http://127.0.0.1:3737";
const actionTypes = ["open_app","set_volume","play_sound","say_sound","ask","wait_for_reply","url","hotkey","emit_event","http_request","shell"];
const actionLabels = {
  open_app: "Открыть приложение",
  set_volume: "Изменить громкость",
  play_sound: "Проиграть звук",
  say_sound: "Озвучка",
  ask: "Задать вопрос",
  wait_for_reply: "Ждать ответ",
  url: "Открыть сайт",
  hotkey: "Нажать клавиши",
  emit_event: "Событие",
  http_request: "HTTP-запрос",
  shell: "Команда shell"
};
const quickStepTypes = ["open_app", "set_volume", "play_sound", "ask", "url"];
const batterySoundSlots = [
  ["battery_0_10", "Заряд 0-10%"],
  ["battery_10_20", "Заряд 10-20%"],
  ["battery_20_30", "Заряд 20-30%"],
  ["battery_30_40", "Заряд 30-40%"],
  ["battery_40_50", "Заряд 40-50%"],
  ["battery_50_60", "Заряд 50-60%"],
  ["battery_60_70", "Заряд 60-70%"],
  ["battery_70_80", "Заряд 70-80%"],
  ["battery_80_90", "Заряд 80-90%"],
  ["battery_90_100", "Заряд 90-99%"],
  ["battery_100", "Заряд 100%"]
];
const powerSoundSlots = [
  ["power_connected", "Зарядку подключили"],
  ["power_disconnected", "Зарядку отключили"],
];
const batteryStatusSoundSlots = [
  ["battery_unavailable", "Не удалось узнать заряд"],
  ...batterySoundSlots
];
const state = { view: "scenarios", scenarios: [], selected: null, document: null, apps: [], status: "", errors: [], run: null, systemSounds: {}, config: null, lmStudioStatus: null };

const $ = (sel) => document.querySelector(sel);
const el = (tag, attrs = {}, children = []) => {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2).toLowerCase(), value);
    else if (typeof value === "boolean") {
      if (value) node.setAttribute(key, "");
    }
    else if (value !== undefined && value !== null) node.setAttribute(key, value);
  }
  for (const child of [].concat(children)) node.append(child?.nodeType ? child : document.createTextNode(child ?? ""));
  return node;
};

async function api(path, options = {}) {
  let lastError;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      const response = await fetch(`${API}${path}`, { headers: { "Content-Type": "application/json" }, ...options });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw Object.assign(new Error(body.error || "request failed"), { body });
      return body;
    } catch (error) {
      lastError = error;
      const tauriPayload = await apiViaTauri(path, options).catch(() => null);
      if (tauriPayload) return tauriPayload;
      if (options.noRetry) break;
      await delay(250);
    }
  }
  throw Object.assign(new Error(`KOMP daemon unavailable: ${lastError?.message || "request failed"}`), { body: lastError?.body });
}

async function apiViaTauri(path, options = {}) {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) throw new Error("tauri invoke unavailable");
  const method = options.method || "GET";
  const body = options.body ? JSON.parse(options.body) : null;
  return invoke("http_json", { method, path, body });
}

function emptyDoc() {
  return {
    manifest_id: "user_new_scenario",
    manifest_name: "User scenario",
    enabled: true,
    scenario: {
      id: "new_scenario",
      aliases: [],
      patterns: [],
      priority: 20,
      sounds: {},
      steps: [{ id: "open_app", action: { type: "open_app", app: "" } }]
    }
  };
}

async function boot() {
  renderShell();
  setStatus("Подключение к KOMP daemon...");
  await Promise.all([loadScenarios(), loadApps(), loadLogo(), loadConfig()]);
}

async function loadScenarios() {
  try {
    state.scenarios = (await api("/scenarios")).scenarios;
    if (!state.selected && state.scenarios[0]) await selectScenario(state.scenarios[0].id);
    setStatus("Готово");
    render();
  } catch (err) {
    state.scenarios = [];
    state.document ||= emptyDoc();
    setStatus(err.message);
    render();
  }
}

async function loadApps() {
  try { state.apps = (await api("/apps")).apps; render(); } catch (_) {}
}

async function loadConfig() {
  try { state.config = await api("/config"); render(); } catch (_) {}
}

async function loadLogo() {
  try {
    const logo = await api("/logo");
    const box = document.querySelector(".logo");
    if (box && logo.data_url) box.replaceChildren(el("img", { src: logo.data_url, alt: "KOMP" }));
  } catch (_) {}
}

async function selectScenario(id) {
  const payload = await api(`/scenarios/${encodeURIComponent(id)}`);
  state.view = "scenarios";
  state.selected = payload.readonly ? null : id;
  state.document = payload.scenario;
  if (payload.readonly) {
    state.document.readonly = false;
    state.document.source_readonly = true;
    state.document.scenario.id = uniqueCopyId(id);
    state.document.manifest_id = `user_${state.document.scenario.id}`;
    state.document.manifest_name = `${state.document.manifest_name} copy`;
    state.status = "Открыта редактируемая копия системного сценария";
  } else {
    state.document.readonly = false;
    state.document.source_readonly = false;
  }
  state.errors = [];
  state.run = null;
  render();
}

function renderShell() {
  $("#app").replaceChildren(
    el("div", { class: "shell" }, [
      el("aside", { class: "sidebar" }, [
        el("div", { class: "brand" }, [el("div", { class: "logo" }, ["K"]), el("div", {}, ["KOMP"])]),
        el("div", { class: "toolbar" }, [
          el("button", { class: "btn primary", onClick: newScenario }, ["Новый сценарий"]),
          el("button", { class: "btn", onClick: openSoundSettings }, ["Озвучка"]),
          el("button", { class: "btn", onClick: openLmStudioSettings }, ["LM Studio"]),
          el("button", { class: "btn", onClick: loadScenarios }, ["Обновить"])
        ]),
        el("input", { class: "search", placeholder: "Поиск сценариев", onInput: render }),
        el("div", { id: "scenarioList", class: "list" })
      ]),
      el("section", { class: "content" }, [
        el("div", { class: "topline" }, [
          el("div", { class: "status", id: "status" }),
          el("div", { class: "row", id: "topActions" })
        ]),
        el("div", { id: "editor" })
      ])
    ])
  );
}

function render() {
  renderSidebar();
  renderTopActions();
  $("#status").textContent = state.status;
  if (state.view === "sounds") renderSoundSettings();
  else if (state.view === "lmstudio") renderLmStudioSettings();
  else renderEditor();
}

function renderChrome() {
  renderSidebar();
  renderTopActions();
  $("#status").textContent = state.status;
}

function renderSidebar() {
  const q = $(".search")?.value?.toLowerCase() || "";
  $("#scenarioList")?.replaceChildren(
    el("button", { class: `scenario-item ${state.view === "sounds" ? "active" : ""}`, onClick: openSoundSettings }, [
      el("div", { class: "scenario-title" }, ["Системная озвучка"]),
      el("div", { class: "scenario-meta" }, ["зарядка, батарея, системные звуки"])
    ]),
    el("button", { class: `scenario-item ${state.view === "lmstudio" ? "active" : ""}`, onClick: openLmStudioSettings }, [
      el("div", { class: "scenario-title" }, ["LM Studio"]),
      el("div", { class: "scenario-meta" }, [state.config?.lmstudio?.enabled ? "fallback включён" : "fallback выключен"])
    ]),
    ...state.scenarios.filter(s => s.id.toLowerCase().includes(q) || s.aliases.join(" ").toLowerCase().includes(q)).map(s =>
    el("button", { class: `scenario-item ${s.id === state.selected ? "active" : ""}`, onClick: () => selectScenario(s.id) }, [
      el("div", { class: "scenario-title" }, [s.id]),
      el("div", { class: "scenario-meta" }, [`${s.step_count} шагов · ${s.readonly ? "system" : "user"}`])
    ])
  ));
}

function renderTopActions() {
  const box = $("#topActions");
  if (!box) return;
  if (state.view === "sounds") {
    box.replaceChildren(el("button", { class: "btn", onClick: loadScenarios }, ["Обновить"]));
    return;
  }
  if (state.view === "lmstudio") {
    box.replaceChildren(
      el("button", { class: "btn", onClick: testLmStudio }, ["Проверить"]),
      el("button", { class: "btn primary", onClick: saveLmStudioSettings }, ["Сохранить"])
    );
    return;
  }
  box.replaceChildren(
    el("button", { class: "btn", onClick: validate }, ["Проверить"]),
    el("button", { class: "btn", onClick: dryRun }, ["Тест без запуска"]),
    el("button", { class: "btn primary", onClick: save }, ["Сохранить"]),
    el("button", { class: "btn danger", onClick: removeScenario }, ["Удалить"])
  );
}

function renderEditor() {
  const root = $("#editor");
  if (!root || !state.document) return;
  const doc = state.document, sc = doc.scenario, readonly = false;
  root.replaceChildren(el("div", { class: "grid" }, [
    el("div", {}, [
      panel("Сценарий", [
        doc.source_readonly ? el("span", { class: "readonly" }, ["копия system-сценария"]) : "",
        help("Сценарий запускается, когда KOMP услышит одну из фраз ниже. Можно вставить сразу много строк."),
        el("div", { class: "form-grid two" }, [
          field("Название сценария", input(doc.manifest_name, v => doc.manifest_name = v, readonly, "text", "Например: Открыть Discord")),
          field("ID для файла", input(sc.id, v => { sc.id = safeId(v); doc.manifest_id = `user_${sc.id}`; }, readonly, "text", "open_discord"))
        ]),
        field("Фразы", el("textarea", { disabled: readonly, placeholder: "открыть дискорд\nзапусти discord\nвключи музыку", onBlur: e => { addAliases(e.target.value); e.target.value = ""; } })),
        el("div", { class: "chips" }, sc.aliases.map((a, i) => el("span", { class: "chip" }, [a, readonly ? "" : el("button", { title: "Удалить фразу", onClick: () => { sc.aliases.splice(i,1); render(); } }, ["×"])]))),
        details("Дополнительно", [
          el("div", { class: "form-grid" }, [
            field("Priority", input(sc.priority, v => sc.priority = Number(v || 0), readonly, "number")),
            field("Regex patterns", input((sc.patterns || []).join(" | "), v => sc.patterns = v.split("|").map(x => x.trim()).filter(Boolean), readonly))
          ])
        ])
      ]),
      panel("Озвучка сценария", [
        help("Звуки можно загрузить сюда. Они сохранятся в папку этого сценария."),
        el("div", { class: "form-grid" }, [
          soundField("Когда сценарий начался", sc.sounds ||= {}, "wake", readonly),
          soundField("Когда слушает уточнение", sc.sounds ||= {}, "listening", readonly),
          soundField("Успех", sc.sounds ||= {}, "success", readonly),
          soundField("Ошибка", sc.sounds ||= {}, "error", readonly),
          soundField("Таймаут", sc.sounds ||= {}, "timeout", readonly)
        ])
      ]),
      panel("Шаги", [
        help("Шаги выполняются сверху вниз. Для обычного сценария чаще всего достаточно одного шага: открыть приложение или сайт."),
        el("div", { class: "steps" }, sc.steps.map((step, i) => renderStep(step, i, readonly))),
        el("div", { class: "row" }, quickStepTypes.map(type => el("button", { class: "btn", disabled: readonly, onClick: () => addStep(type) }, [`+ ${actionLabels[type]}`])))
      ])
    ]),
    el("div", {}, [
      panel("Flow", [el("div", { class: "flow" }, sc.steps.map(s => el("div", { class: "flow-row" }, [`${s.id} → ${s.on_success || "next"} / ${s.on_error || "next"}`])))]),
      panel("Validation", [state.errors.length ? el("div", { class: "errors" }, state.errors.map(e => el("div", {}, [e]))) : el("div", { class: "ok" }, ["OK"])]),
      panel("Dry run", [el("pre", { class: "run" }, [state.run ? JSON.stringify(state.run, null, 2) : ""])])
    ])
  ]));
}

function renderSoundSettings() {
  const root = $("#editor");
  if (!root) return;
  root.replaceChildren(el("div", { class: "grid" }, [
    el("div", {}, [
      panel("Системная озвучка", [
        help("Эти звуки не относятся к сценариям. KOMP проигрывает их сам, когда меняется питание ноутбука."),
        el("div", { class: "form-grid two" }, powerSoundSlots.map(([slot, label]) => systemSoundField(label, slot)))
      ]),
      panel("Озвучка уровня батареи", [
        help("Эти фразы проигрываются только по команде проверки заряда, например “сколько зарядки”."),
        el("div", { class: "form-grid" }, batteryStatusSoundSlots.map(([slot, label]) => systemSoundField(label, slot)))
      ])
    ]),
    el("div", {}, [
      panel("Где лежат файлы", [
        help("Загруженные файлы сохраняются в sounds/system. Подключение и отключение зарядки мониторятся постоянно, пока daemon запущен.")
      ])
    ])
  ]));
}

function renderLmStudioSettings() {
  const root = $("#editor");
  if (!root) return;
  const config = ensureConfig();
  const lm = config.lmstudio;
  root.replaceChildren(el("div", { class: "grid" }, [
    el("div", {}, [
      panel("LM Studio", [
        help("KOMP использует LM Studio только как fallback, когда локальные сценарии не распознали команду."),
        el("div", { class: "form-grid two" }, [
          field("Включить fallback", checkbox(!!lm.enabled, v => lm.enabled = v)),
          field("Base URL", input(lm.base_url || "http://localhost:1234/v1", v => lm.base_url = v, false, "text", "http://localhost:1234/v1")),
          field("Model", lmStudioModelField(lm)),
          field("Timeout, ms", input(lm.timeout_ms ?? 2500, v => lm.timeout_ms = Number(v || 2500), false, "number", "2500")),
          field("Min confidence", input(lm.min_confidence ?? 0.55, v => lm.min_confidence = Number(v || 0.55), false, "number", "0.55"))
        ])
      ]),
      panel("Статус подключения", [
        state.lmStudioStatus ? renderLmStudioStatus(state.lmStudioStatus) : help("Нажми “Проверить”, чтобы убедиться, что LM Studio server запущен и OpenAI-compatible API доступен.")
      ])
    ]),
    el("div", {}, [
      panel("Как подключить", [
        help("В LM Studio открой Local Server, включи OpenAI-compatible server, оставь порт 1234 или укажи свой URL здесь."),
        help("Если модель найдена, выбери её в поле Model и нажми “Сохранить”.")
      ])
    ])
  ]));
}

function renderStep(step, index, readonly) {
  const ids = state.document.scenario.steps.map(s => s.id);
  return el("div", { class: "step" }, [
    el("div", { class: "step-head" }, [
      field(`Шаг ${index + 1}`, input(step.id, v => step.id = safeId(v), readonly, "text", "open_discord")),
      field("Что сделать", select(actionTypes, step.action.type, v => step.action = defaultAction(v), readonly, true, actionLabels)),
      el("button", { class: "btn danger", disabled: readonly, onClick: () => { state.document.scenario.steps.splice(index, 1); render(); } }, ["Удалить"])
    ]),
    el("div", { class: "action-fields" }, actionFields(step.action, readonly)),
    details("Логика и звуки шага", [
      help("Условия нужны для разветвлений: например, после вопроса открыть Chrome, если ответ содержит chrome."),
      el("div", { class: "conditions" }, [
        field("Слот ответа", input(step.when?.slot || "", v => setWhen(step, "slot", v), readonly, "text", "browser")),
        field("Содержит текст", input(step.when?.contains || "", v => setWhen(step, "contains", v), readonly, "text", "chrome")),
        field("Последний ответ содержит", input(step.when?.reply_contains || "", v => setWhen(step, "reply_contains", v), readonly)),
        field("Только для ОС", select(["","macos","windows","linux"], step.when?.os || "", v => setWhen(step, "os", v), readonly))
      ]),
      el("div", { class: "branches" }, [
        field("Если успешно, перейти к", select(["", ...ids], step.on_success || "", v => step.on_success = v || undefined, readonly)),
        field("Если ошибка, перейти к", select(["", ...ids], step.on_error || "", v => step.on_error = v || undefined, readonly))
      ]),
      el("div", { class: "branches" }, [
        soundField("Звук перед шагом", step, "before_sound", readonly),
        soundField("Звук после шага", step, "after_sound", readonly)
      ])
    ])
  ]);
}

function actionFields(a, ro) {
  const appOptions = ["", ...state.apps.map(app => app.name)];
  if (a.type === "open_app") return [field("Выбрать приложение", select(appOptions, a.app || "", v => a.app = v, ro)), field("Или написать вручную", input(a.app || "", v => a.app = v, ro, "text", "Discord"))];
  if (a.type === "set_volume") return [field("Поставить уровень 0-100", input(a.level ?? "", v => a.level = v === "" ? undefined : Number(v), ro, "number")), field("Изменить на", input(a.delta ?? "", v => a.delta = v === "" ? undefined : Number(v), ro, "number", "-15"))];
  if (a.type === "play_sound" || a.type === "say_sound") return [soundField("Файл", a, "file", ro)];
  if (a.type === "ask") return [soundField("Звук вопроса", a, "sound", ro), field("Куда сохранить ответ", input(a.reply_slot || "", v => a.reply_slot = v, ro, "text", "browser"))];
  if (a.type === "wait_for_reply") return [field("Куда сохранить ответ", input(a.reply_slot || "", v => a.reply_slot = v, ro, "text", "answer"))];
  if (a.type === "url") return [field("Ссылка", input(a.url || "", v => a.url = v, ro, "text", "https://discord.com"))];
  if (a.type === "hotkey") return [field("Клавиши через +", input((a.keys || []).join("+"), v => a.keys = v.split("+").map(x => x.trim()).filter(Boolean), ro, "text", "cmd+space"))];
  if (a.type === "emit_event") return [field("Название события", input(a.event || "", v => a.event = v, ro))];
  if (a.type === "http_request") return [field("Method", input(a.method || "GET", v => a.method = v, ro)), field("URL", input(a.url || "", v => a.url = v, ro))];
  if (a.type === "shell") return [field("Command", input(a.command || "", v => a.command = v, ro)), field("Enabled", select(["false","true"], String(!!a.enabled), v => a.enabled = v === "true", ro))];
  return [];
}

function panel(title, children) { return el("section", { class: "panel" }, [el("h2", {}, [title]), ...children]); }
function field(labelText, child) { return el("label", { class: "field" }, [el("span", {}, [labelText]), child]); }
function help(text) { return el("p", { class: "hint" }, [text]); }
function details(title, children) { return el("details", { class: "advanced" }, [el("summary", {}, [title]), ...children]); }
function checkbox(checked, onChange) {
  const box = el("input", { type: "checkbox", onChange: e => { onChange(e.target.checked); renderChrome(); } });
  box.checked = checked;
  return box;
}
function lmStudioModelField(lm) {
  const models = state.lmStudioStatus?.models || [];
  if (models.length) {
    return select(["", ...models], lm.model || "", v => lm.model = v || null, false);
  }
  return input(lm.model || "", v => lm.model = v || null, false, "text", "local-model");
}
function renderLmStudioStatus(status) {
  return el("div", { class: status.ok ? "ok" : "errors" }, [
    status.ok ? "Подключено" : "Не подключено",
    status.models?.length ? el("div", { class: "scenario-meta" }, [`Модели: ${status.models.join(", ")}`]) : "",
    status.error ? el("div", { class: "scenario-meta" }, [status.error]) : ""
  ]);
}
function soundField(labelText, target, key, disabled) {
  return field(labelText, el("div", { class: "sound-row" }, [
    input(target[key] || "", v => target[key] = v || undefined, disabled),
    el("label", { class: "btn file-button" }, [
      "Загрузить",
      el("input", { type: "file", accept: ".mp3,.wav,.ogg,audio/mpeg,audio/wav,audio/ogg", disabled, onChange: e => uploadSound(e.target.files?.[0], target, key) })
    ])
  ]));
}
function systemSoundField(labelText, slot) {
  return field(labelText, el("div", { class: "sound-row" }, [
    input(state.systemSounds[slot] || "", v => state.systemSounds[slot] = v, false, "text", `sounds/system/${slot}.mp3`),
    el("label", { class: "btn file-button" }, [
      "Загрузить",
      el("input", { type: "file", accept: ".mp3,.wav,.ogg,audio/mpeg,audio/wav,audio/ogg", onChange: e => uploadSystemSound(e.target.files?.[0], slot) })
    ])
  ]));
}
function input(value, onChange, disabled, type = "text", placeholder = "") { return el("input", { type, value: value ?? "", placeholder, disabled, onInput: e => { onChange(e.target.value); renderChrome(); } }); }
function select(options, value, onChange, disabled, rerender = false, labels = {}) { const s = el("select", { disabled, onChange: e => { onChange(e.target.value); rerender ? render() : renderChrome(); } }, options.map(o => el("option", { value: o }, [labels[o] || o || "—"]))); s.value = value ?? ""; return s; }
function safeId(v) { return String(v).trim().replace(/\s+/g, "_").replace(/[^\w-]/g, ""); }
function uniqueCopyId(id) {
  const base = safeId(`${id}_copy`) || "scenario_copy";
  const used = new Set(state.scenarios.map(s => s.id));
  if (!used.has(base)) return base;
  for (let i = 2; i < 1000; i += 1) {
    const candidate = `${base}_${i}`;
    if (!used.has(candidate)) return candidate;
  }
  return `${base}_${Date.now()}`;
}
function setWhen(step, key, value) { step.when ||= {}; value ? step.when[key] = value : delete step.when[key]; if (!Object.keys(step.when).length) delete step.when; }
function addAliases(text) { const lines = text.split(/\n+/).map(s => s.trim()).filter(Boolean); state.document.scenario.aliases = [...new Set([...state.document.scenario.aliases, ...lines])]; render(); }
function addStep(type = "open_app") { state.document.scenario.steps.push({ id: `step_${state.document.scenario.steps.length + 1}`, action: defaultAction(type) }); render(); }
function defaultAction(type) {
  return { open_app:{type,app:""}, set_volume:{type,delta:-10}, play_sound:{type,file:"sounds/system/listening.mp3"}, say_sound:{type,file:"sounds/system/listening.mp3"}, ask:{type,reply_slot:"reply"}, wait_for_reply:{type,reply_slot:"reply"}, url:{type,url:"https://"}, hotkey:{type,keys:[]}, emit_event:{type,event:"event_name",payload:{}}, http_request:{type,method:"GET",url:"https://"}, shell:{type,command:"",args:[],enabled:false} }[type];
}
function newScenario() { state.view = "scenarios"; state.selected = null; state.document = emptyDoc(); state.errors = []; state.run = null; render(); }
function openSoundSettings() { state.view = "sounds"; state.selected = null; state.errors = []; state.run = null; setStatus("Системная озвучка"); render(); }
function openLmStudioSettings() { state.view = "lmstudio"; state.selected = null; state.errors = []; state.run = null; ensureConfig(); setStatus("LM Studio"); render(); }
function ensureConfig() {
  state.config ||= {
    wake_phrase: "комп",
    wake_phrases: ["комп", "компьютер"],
    wake_grammar: ["комп", "компьютер"],
    primary_language: "ru",
    english_fallback: true,
    models: { ru_vosk_path: null, en_vosk_path: null },
    lmstudio: {},
    whisper: { enabled: false, cli_path: null, model_path: null, language: "ru", timeout_ms: 8000, extra_args: ["-nt"] },
    audio: { sample_rate_hz: 16000, command_timeout_ms: 5000, end_silence_ms: 700, command_preroll_ms: 300 },
    sounds: {},
    plugin_dirs: ["plugins.example"]
  };
  state.config.lmstudio ||= {};
  state.config.lmstudio.enabled ??= true;
  state.config.lmstudio.base_url ||= "http://localhost:1234/v1";
  state.config.lmstudio.timeout_ms ||= 2500;
  state.config.lmstudio.min_confidence ??= 0.55;
  return state.config;
}
function setStatus(text) { state.status = text; render(); }
function delay(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }

async function validate() {
  if (!state.document) return newScenario();
  try { const r = await api(`/scenarios/${state.document.scenario.id}/validate`, { method:"POST", body: JSON.stringify(state.document) }); state.errors = r.errors || []; setStatus("Проверено"); }
  catch (e) { state.errors = e.body?.errors || [e.message]; render(); }
}
async function dryRun() {
  if (!state.document) return newScenario();
  try { const r = await api(`/scenarios/${state.document.scenario.id}/dry-run`, { method:"POST" }); state.run = r.run; setStatus("Dry run готов"); render(); }
  catch (e) { state.errors = [e.message]; render(); }
}
async function save() {
  if (!state.document) return newScenario();
  const method = state.scenarios.some(s => s.id === state.document.scenario.id && !s.readonly) ? "PUT" : "POST";
  const path = method === "POST" ? "/scenarios" : `/scenarios/${state.document.scenario.id}`;
  try { await api(path, { method, body: JSON.stringify(state.document) }); setStatus("Сохранено"); await loadScenarios(); await selectScenario(state.document.scenario.id); }
  catch (e) { state.errors = e.body?.errors || [e.message]; render(); }
}
async function removeScenario() {
  if (!state.selected || state.document?.readonly) return;
  try { await api(`/scenarios/${state.selected}`, { method:"DELETE" }); state.selected = null; state.document = null; await loadScenarios(); }
  catch (e) { state.errors = [e.message]; render(); }
}
async function saveLmStudioSettings() {
  const config = ensureConfig();
  try {
    state.config = await api("/config", { method: "POST", body: JSON.stringify(config) });
    setStatus("LM Studio настройки сохранены");
  } catch (e) {
    state.errors = [e.message];
    render();
  }
}
async function testLmStudio() {
  const lm = ensureConfig().lmstudio;
  setStatus("Проверяю LM Studio...");
  try {
    const result = await api("/lmstudio/test", { method: "POST", body: JSON.stringify(lm) });
    state.lmStudioStatus = result;
    setStatus(result.ok ? "LM Studio подключен" : "LM Studio не отвечает");
    render();
  } catch (e) {
    state.lmStudioStatus = { ok: false, models: [], error: e.message };
    setStatus("LM Studio не отвечает");
    render();
  }
}
async function uploadSound(file, target, key) {
  if (!file || !state.document?.scenario?.id) return;
  setStatus("Загружаю звук...");
  try {
    const dataUrl = await readFileDataUrl(file);
    const data_base64 = dataUrl.split(",")[1] || "";
    const result = await api(`/scenarios/${state.document.scenario.id}/sounds`, {
      method: "POST",
      body: JSON.stringify({ file_name: file.name, data_base64 })
    });
    target[key] = result.file;
    setStatus("Звук загружен");
  } catch (e) {
    const message = uploadErrorMessage(e);
    state.errors = [message];
    setStatus(message);
    render();
  }
}
async function uploadSystemSound(file, slot) {
  if (!file) return;
  setStatus("Загружаю системную озвучку...");
  try {
    const dataUrl = await readFileDataUrl(file);
    const data_base64 = dataUrl.split(",")[1] || "";
    const result = await api(`/system-sounds/${encodeURIComponent(slot)}`, {
      method: "POST",
      body: JSON.stringify({ file_name: file.name, data_base64 })
    });
    state.systemSounds[slot] = result.file;
    setStatus("Озвучка батареи загружена");
  } catch (e) {
    const message = uploadErrorMessage(e);
    state.errors = [message];
    setStatus(message);
    render();
  }
}
function uploadErrorMessage(error) {
  const message = error?.message || "Не удалось загрузить звук";
  if (message.includes("KOMP daemon unavailable") || message.includes("Not Found") || message.includes("request failed")) {
    return `${message}. Перезапусти KOMP daemon, если он был запущен до обновления.`;
  }
  return message;
}
function readFileDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ""));
    reader.onerror = () => reject(reader.error || new Error("file read failed"));
    reader.readAsDataURL(file);
  });
}

boot();
