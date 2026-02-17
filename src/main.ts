type AppPromptEntry = {
  bundleId: string;
  context: string;
};

type UrgencyLevel = "critical" | "high" | "medium" | "low";

type UiNotification = {
  id: number;
  title: string;
  body: string;
  subtitle: string;
  bundleId: string;
  appName: string;
  urgencyLevel: UrgencyLevel;
  urgencyLabel: string;
  urgencyColor: string;
  summaryLine: string;
  reason: string;
};

type UiNotificationGroup = {
  bundleId: string;
  appName: string;
  notifications: UiNotification[];
  hiddenCount: number;
};

type TauriEvent<T = unknown> = {
  payload: T;
};

type TauriGlobal = {
  invoke?: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  tauri?: {
    invoke?: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  };
  core?: {
    invoke?: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  };
  event?: {
    listen?: <T>(
      event: string,
      handler: (event: TauriEvent<T>) => void,
    ) => Promise<() => void>;
  };
};

declare global {
  interface Window {
    __TAURI__?: TauriGlobal;
  }
}

function requireRoot(): HTMLDivElement {
  const element = document.querySelector<HTMLDivElement>("#app");
  if (!element) {
    throw new Error("#app not found");
  }
  return element;
}

const root = requireRoot();

const state: {
  groups: UiNotificationGroup[];
  selected: UiNotification | null;
  error: string;
  loading: boolean;
  view: "notifications" | "settings";
  prompts: AppPromptEntry[];
  editingPrompt: { bundleId: string; context: string; isNew: boolean } | null;
  ignoredApps: string[];
  confirm: { message: string; okLabel?: string; onOk: () => void } | null;
} = {
  groups: [],
  selected: null,
  error: "",
  loading: false,
  view: "notifications",
  prompts: [],
  editingPrompt: null,
  ignoredApps: [],
  confirm: null,
};

async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const invoke =
    window.__TAURI__?.invoke ??
    window.__TAURI__?.tauri?.invoke ??
    window.__TAURI__?.core?.invoke;
  if (!invoke) {
    throw new Error("Tauri runtime is not available");
  }
  return invoke<T>(command, args);
}

function create<K extends keyof HTMLElementTagNameMap>(
  tagName: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tagName);
  if (className) {
    element.className = className;
  }
  if (text !== undefined) {
    element.textContent = text;
  }
  return element;
}

function urgencyBadgeStyle(color: string): string {
  return `background:${color};box-shadow:0 0 10px ${color}44`;
}

function render(): void {
  root.replaceChildren();

  const panel = create("main", "panel");
  const header = create("header", "panel-header");
  const title = create(
    "h1",
    "panel-title",
    state.view === "notifications" ? "通知インボックス" : "設定",
  );

  const actions = create("div", "panel-actions");

  if (state.view === "notifications") {
    const refreshBtn = create("button", "btn secondary", "更新");
    refreshBtn.addEventListener("click", () => {
      void loadGroups();
    });

    const dummyBtn = create("button", "btn secondary", "ダミー投入");
    dummyBtn.addEventListener("click", async () => {
      await injectDummy();
    });

    const clearAllBtn = create("button", "btn warn", "全通知をクリア");
    clearAllBtn.addEventListener("click", () => {
      state.confirm = {
        message: "全通知をクリアしますか？",
        okLabel: "クリア",
        onOk: async () => {
          await clearAll();
        },
      };
      render();
    });

    actions.append(refreshBtn, dummyBtn, clearAllBtn);
  }

  const settingsBtn = create(
    "button",
    `btn ${state.view === "settings" ? "warn" : "secondary"}`,
    state.view === "settings" ? "戻る" : "設定",
  );
  settingsBtn.addEventListener("click", () => {
    if (state.view === "notifications") {
      state.view = "settings";
      state.editingPrompt = null;
      void loadPrompts();
    } else {
      state.view = "notifications";
    }
    render();
  });
  actions.append(settingsBtn);

  header.append(title, actions);

  if (state.view === "notifications") {
    const groups = create("section", "groups");
    if (!state.loading && state.groups.length === 0) {
      groups.append(
        create("div", "empty", "現在表示できる通知はありません。"),
      );
    }

    let groupIdx = 0;
    for (const group of state.groups) {
      const section = create("section", "group");
      section.style.animationDelay = `${groupIdx * 0.06}s`;
      const groupHeader = create("div", "group-header");
      const groupTitle = create(
        "h2",
        "group-title",
        `${group.appName} (${group.notifications.length + group.hiddenCount})`,
      );

      const groupActions = create("div", "group-actions");

      const promptBtn = create("button", "group-clear-btn");
      promptBtn.title = "このアプリのプロンプトを設定";
      promptBtn.innerHTML =
        '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M11.33 2a1.89 1.89 0 0 1 2.67 2.67L5.33 13.33 1.33 14.67l1.34-4L11.33 2z"/></svg>';
      promptBtn.addEventListener("click", async () => {
        state.view = "settings";
        state.error = "";
        await loadPrompts();
        const existing = state.prompts.find(
          (p) => p.bundleId === group.bundleId,
        );
        state.editingPrompt = {
          bundleId: group.bundleId,
          context: existing?.context ?? "",
          isNew: !existing,
        };
        render();
      });

      const ignoreBtn = create("button", "group-clear-btn");
      ignoreBtn.title = "このアプリを無視";
      ignoreBtn.innerHTML =
        '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M1 1l14 14"/><path d="M6.5 6.5a2 2 0 0 0 3 3"/><path d="M2.5 2.5C1.4 3.7.5 5.2.5 8c0 4.5 7.5 7 7.5 7s2.3-.8 4.5-2.5"/><path d="M14 11.2C15 9.8 15.5 8.5 15.5 8c0-4.5-7.5-7-7.5-7-.8.4-1.7 1-2.5 1.6"/></svg>';
      ignoreBtn.addEventListener("click", () => {
        state.confirm = {
          message: `${group.appName} の通知を今後無視しますか？`,
          okLabel: "無視する",
          onOk: async () => {
            await addIgnoredApp(group.bundleId);
            await clearApp(group.bundleId);
          },
        };
        render();
      });

      const clearAppBtn = create("button", "group-clear-btn");
      clearAppBtn.title = "このアプリをクリア";
      clearAppBtn.innerHTML =
        '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h12M5.33 4V2.67a1.33 1.33 0 0 1 1.34-1.34h2.66a1.33 1.33 0 0 1 1.34 1.34V4M6.67 7.33v4M9.33 7.33v4"/><path d="M3.33 4h9.34l-.67 9.33a1.33 1.33 0 0 1-1.33 1.34H5.33A1.33 1.33 0 0 1 4 13.33L3.33 4z"/></svg>';
      clearAppBtn.addEventListener("click", async () => {
        await clearApp(group.bundleId);
      });

      groupActions.append(promptBtn, ignoreBtn, clearAppBtn);
      groupHeader.append(groupTitle, groupActions);

      const cards = create("div", "cards");
      let cardIdx = 0;
      for (const notification of group.notifications) {
        const card = renderCard(notification);
        card.style.animationDelay = `${groupIdx * 0.06 + cardIdx * 0.03}s`;
        cards.append(card);
        cardIdx++;
      }

      if (group.hiddenCount > 0) {
        cards.append(
          create(
            "p",
            "hidden-row",
            `他 ${group.hiddenCount} 件は省略されています`,
          ),
        );
      }

      section.append(groupHeader, cards);
      groups.append(section);
      groupIdx++;
    }

    panel.append(header, groups);
  } else {
    panel.append(header, renderSettingsView());
  }

  if (state.error) {
    panel.append(create("p", "error", state.error));
  }

  root.append(panel);

  if (state.selected) {
    root.append(renderDialog(state.selected));
  }

  if (state.confirm) {
    root.append(renderConfirmDialog(state.confirm));
  }
}

function renderConfirmDialog(confirm: {
  message: string;
  okLabel?: string;
  onOk: () => void;
}): HTMLElement {
  const overlay = create("div", "overlay");
  overlay.addEventListener("click", (event) => {
    if (event.target === overlay) {
      state.confirm = null;
      render();
    }
  });

  const dialog = create("article", "dialog");
  dialog.style.width = "min(360px, 88vw)";

  const msg = create("p", "dialog-section", confirm.message);
  msg.style.margin = "8px 0 16px";

  const actions = create("div", "panel-actions");
  actions.style.justifyContent = "flex-end";

  const cancelBtn = create("button", "btn secondary", "キャンセル");
  cancelBtn.addEventListener("click", () => {
    state.confirm = null;
    render();
  });

  const okBtn = create("button", "btn warn", confirm.okLabel ?? "OK");
  okBtn.addEventListener("click", () => {
    state.confirm = null;
    confirm.onOk();
  });

  actions.append(cancelBtn, okBtn);
  dialog.append(msg, actions);
  overlay.append(dialog);
  return overlay;
}

function renderCard(notification: UiNotification): HTMLElement {
  const card = create("article", "card");

  const bar = create("div", "card-bar");
  bar.style.background = notification.urgencyColor;
  bar.style.boxShadow = `0 0 8px ${notification.urgencyColor}40`;

  const openBtn = create("button", "card-main");
  openBtn.type = "button";
  openBtn.addEventListener("click", () => {
    state.selected = notification;
    render();
  });

  const label = create("span", "card-label", notification.urgencyLabel);
  label.setAttribute("style", urgencyBadgeStyle(notification.urgencyColor));

  const summary = create("p", "card-summary", notification.summaryLine);
  const sub = create(
    "p",
    "card-sub",
    `${notification.title || "タイトルなし"} / ${notification.appName}`,
  );
  openBtn.append(label, summary, sub);

  const openAppBtn = create("button", "card-clear");
  openAppBtn.type = "button";
  openAppBtn.title = "アプリを開く";
  openAppBtn.innerHTML =
    '<svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2H2v12h12v-4"/><path d="M10 2h4v4"/><path d="M16 0L7 9"/></svg>';
  openAppBtn.addEventListener("click", async (event) => {
    event.stopPropagation();
    await invokeCommand("open_app", { bundleId: notification.bundleId });
  });

  const clearBtn = create("button", "card-clear", "×");
  clearBtn.type = "button";
  clearBtn.title = "この通知をクリア";
  clearBtn.addEventListener("click", async (event) => {
    event.stopPropagation();
    await clearOne(notification.id);
  });

  card.append(bar, openBtn, openAppBtn, clearBtn);
  return card;
}

function renderDialog(notification: UiNotification): HTMLElement {
  const overlay = create("div", "overlay");
  overlay.addEventListener("click", (event) => {
    if (event.target === overlay) {
      state.selected = null;
      render();
    }
  });

  const dialog = create("article", "dialog");
  const title = create("h3", "dialog-title", notification.summaryLine);

  const meta = create("div", "dialog-meta");
  const urgency = create("span", "dialog-pill", notification.urgencyLabel);
  urgency.setAttribute("style", urgencyBadgeStyle(notification.urgencyColor));
  const app = create("span", "dialog-pill", notification.appName);
  app.style.background = "#334155";
  meta.append(urgency, app);

  const reasonTitle = create("p", "card-sub", "Gemini判定理由");
  const reason = create("p", "dialog-section", notification.reason);

  const originalTitle = create("p", "card-sub", "元通知");
  const original = create(
    "p",
    "dialog-section",
    [notification.title, notification.subtitle, notification.body]
      .filter(Boolean)
      .join("\n\n"),
  );

  const actions = create("div", "dialog-actions");
  const closeBtn = create("button", "dialog-icon-btn", "←");
  closeBtn.title = "閉じる";
  closeBtn.addEventListener("click", () => {
    state.selected = null;
    render();
  });

  const openAppBtn = create("button", "dialog-icon-btn");
  openAppBtn.innerHTML =
    '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2H2v12h12v-4"/><path d="M10 2h4v4"/><path d="M16 0L7 9"/></svg>';
  openAppBtn.title = "アプリを開く";
  openAppBtn.addEventListener("click", async () => {
    await invokeCommand("open_app", { bundleId: notification.bundleId });
  });

  const clearBtn = create("button", "dialog-icon-btn warn");
  clearBtn.innerHTML =
    '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h12M5.33 4V2.67a1.33 1.33 0 0 1 1.34-1.34h2.66a1.33 1.33 0 0 1 1.34 1.34V4M6.67 7.33v4M9.33 7.33v4"/><path d="M3.33 4h9.34l-.67 9.33a1.33 1.33 0 0 1-1.33 1.34H5.33A1.33 1.33 0 0 1 4 13.33L3.33 4z"/></svg>';
  clearBtn.title = "この通知をクリア";
  clearBtn.addEventListener("click", async () => {
    await clearOne(notification.id);
    state.selected = null;
    render();
  });

  actions.append(closeBtn, openAppBtn, clearBtn);
  dialog.append(title, meta, reasonTitle, reason, originalTitle, original, actions);
  overlay.append(dialog);
  return overlay;
}

function renderSettingsView(): HTMLElement {
  const container = create("section", "groups");

  const addBtn = create("button", "btn secondary", "追加");
  addBtn.style.marginTop = "12px";
  addBtn.addEventListener("click", () => {
    state.editingPrompt = { bundleId: "", context: "", isNew: true };
    render();
  });

  if (state.editingPrompt) {
    container.append(renderPromptForm(state.editingPrompt));
  }

  if (state.prompts.length === 0 && !state.editingPrompt) {
    container.append(
      create("div", "empty", "アプリプロンプトはまだ登録されていません。"),
    );
  }

  for (const prompt of state.prompts) {
    const row = create("section", "group");
    const rowHeader = create("div", "group-header");
    const rowTitle = create("h2", "group-title", prompt.bundleId);

    const rowActions = create("div", "panel-actions");
    const editBtn = create("button", "group-clear-btn");
    editBtn.title = "編集";
    editBtn.textContent = "✎";
    editBtn.addEventListener("click", () => {
      state.editingPrompt = {
        bundleId: prompt.bundleId,
        context: prompt.context,
        isNew: false,
      };
      render();
    });

    const deleteBtn = create("button", "group-clear-btn");
    deleteBtn.title = "削除";
    deleteBtn.innerHTML =
      '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h12M5.33 4V2.67a1.33 1.33 0 0 1 1.34-1.34h2.66a1.33 1.33 0 0 1 1.34 1.34V4M6.67 7.33v4M9.33 7.33v4"/><path d="M3.33 4h9.34l-.67 9.33a1.33 1.33 0 0 1-1.33 1.34H5.33A1.33 1.33 0 0 1 4 13.33L3.33 4z"/></svg>';
    deleteBtn.addEventListener("click", async () => {
      await deletePrompt(prompt.bundleId);
    });

    rowActions.append(editBtn, deleteBtn);
    rowHeader.append(rowTitle, rowActions);

    const contextPreview = create("p", "card-sub");
    contextPreview.textContent =
      prompt.context.length > 80
        ? prompt.context.slice(0, 80) + "…"
        : prompt.context;
    contextPreview.style.margin = "4px 0 0";

    row.append(rowHeader, contextPreview);
    container.append(row);
  }

  if (!state.editingPrompt) {
    container.append(addBtn);
  }

  // Ignored apps section
  const ignoredSection = create("section", "group");
  ignoredSection.style.marginTop = "16px";
  const ignoredHeader = create("h2", "group-title", "無視アプリ");
  ignoredHeader.style.marginBottom = "4px";
  ignoredSection.append(ignoredHeader);

  if (state.ignoredApps.length === 0) {
    ignoredSection.append(
      create("div", "empty", "無視アプリはまだ登録されていません。"),
    );
  }

  for (const bundleId of state.ignoredApps) {
    const row = create("div", "group-header");
    const label = create("span", "card-sub", bundleId);
    const unignoreBtn = create("button", "btn secondary", "解除");
    unignoreBtn.addEventListener("click", async () => {
      await removeIgnoredApp(bundleId);
    });
    row.append(label, unignoreBtn);
    ignoredSection.append(row);
  }

  container.append(ignoredSection);

  return container;
}

function renderPromptForm(editing: {
  bundleId: string;
  context: string;
  isNew: boolean;
}): HTMLElement {
  const form = create("section", "group");
  form.style.border = "2px solid #93c5fd";

  const formTitle = create(
    "h2",
    "group-title",
    editing.isNew ? "新規プロンプト" : "プロンプト編集",
  );
  formTitle.style.marginBottom = "8px";

  const bundleLabel = create("label", "card-sub", "Bundle ID");
  bundleLabel.style.display = "block";
  bundleLabel.style.margin = "4px 0 2px";
  const bundleInput = document.createElement("input");
  bundleInput.type = "text";
  bundleInput.className = "prompt-input";
  bundleInput.value = editing.bundleId;
  bundleInput.placeholder = "com.example.app";
  bundleInput.disabled = !editing.isNew || editing.bundleId !== "";
  bundleInput.addEventListener("input", () => {
    editing.bundleId = bundleInput.value;
  });

  const contextLabel = create("label", "card-sub", "コンテキスト");
  contextLabel.style.display = "block";
  contextLabel.style.margin = "8px 0 2px";
  const contextInput = document.createElement("textarea");
  contextInput.className = "prompt-textarea";
  contextInput.value = editing.context;
  contextInput.placeholder = "このアプリに関する追加コンテキストを入力…";
  contextInput.rows = 3;
  contextInput.addEventListener("input", () => {
    editing.context = contextInput.value;
  });

  const actions = create("div", "panel-actions");
  actions.style.marginTop = "8px";
  const saveBtn = create("button", "btn secondary", "保存");
  saveBtn.addEventListener("click", async () => {
    if (!editing.bundleId.trim() || !editing.context.trim()) {
      state.error = "Bundle ID とコンテキストは必須です。";
      render();
      return;
    }
    await savePrompt(editing.bundleId.trim(), editing.context.trim());
    state.editingPrompt = null;
  });

  const cancelBtn = create("button", "btn secondary", "キャンセル");
  cancelBtn.addEventListener("click", () => {
    state.editingPrompt = null;
    render();
  });

  actions.append(saveBtn, cancelBtn);
  form.append(
    formTitle,
    bundleLabel,
    bundleInput,
    contextLabel,
    contextInput,
    actions,
  );
  return form;
}

async function loadPrompts(): Promise<void> {
  try {
    state.prompts = await invokeCommand<AppPromptEntry[]>("get_app_prompts");
    state.ignoredApps = await invokeCommand<string[]>("get_ignored_apps");
  } catch (error) {
    state.error = (error as Error).message;
  }
  render();
}

async function savePrompt(
  bundleId: string,
  context: string,
): Promise<void> {
  try {
    state.error = "";
    await invokeCommand("set_app_prompt", { bundleId, context });
    await loadPrompts();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function deletePrompt(bundleId: string): Promise<void> {
  try {
    state.error = "";
    await invokeCommand("delete_app_prompt", { bundleId });
    await loadPrompts();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function addIgnoredApp(bundleId: string): Promise<void> {
  try {
    state.error = "";
    await invokeCommand("add_ignored_app", { bundleId });
    state.ignoredApps = await invokeCommand<string[]>("get_ignored_apps");
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function removeIgnoredApp(bundleId: string): Promise<void> {
  try {
    state.error = "";
    await invokeCommand("remove_ignored_app", { bundleId });
    await loadPrompts();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function loadGroups(): Promise<void> {
  state.loading = true;
  state.error = "";
  render();

  try {
    const groups = await invokeCommand<UiNotificationGroup[]>("get_notification_groups");
    state.groups = groups;
  } catch (error) {
    state.error = (error as Error).message;
  } finally {
    state.loading = false;
    render();
  }
}

async function clearOne(id: number): Promise<void> {
  try {
    state.error = "";
    await invokeCommand<boolean>("clear_notification", { id });
    await loadGroups();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function clearApp(bundleId: string): Promise<void> {
  try {
    state.error = "";
    await invokeCommand<number>("clear_app_notifications", { bundleId });
    await loadGroups();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function clearAll(): Promise<void> {
  try {
    state.error = "";
    await invokeCommand<number>("clear_all_notifications");
    await loadGroups();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function injectDummy(): Promise<void> {
  try {
    state.error = "";
    await invokeCommand<number>("inject_dummy_notifications", { count: 8 });
    await loadGroups();
  } catch (error) {
    state.error = (error as Error).message;
    render();
  }
}

async function setupEventListener(): Promise<void> {
  const listen = window.__TAURI__?.event?.listen;
  if (!listen) {
    return;
  }

  await listen("notifications-updated", () => {
    void loadGroups();
  });
}

void setupEventListener();
void loadGroups();

export {};
