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
  timestamp: number;
};

type UiNotificationGroup = {
  bundleId: string;
  appName: string;
  iconBase64: string | null;
  notifications: UiNotification[];
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

function resetUiToMainView(): void {
  state.view = "notifications";
  state.selected = null;
  state.editingPrompt = null;
  state.confirm = null;
  state.error = "";
}

function resetUiToMainViewAndRender(): void {
  resetUiToMainView();
  render();
}

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

function formatRelativeTime(timestamp: number): string {
  const seconds = Math.floor(Date.now() / 1000) - timestamp;
  if (seconds < 60) return "たった今";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}分前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}時間前`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}日前`;
  const weeks = Math.floor(days / 7);
  return `${weeks}週間前`;
}

function render(): void {
  root.replaceChildren();

  const panel = create("main", "panel");
  const header = create("header", "panel-header");
  header.setAttribute("data-tauri-drag-region", "");
  const title = create(
    "h1",
    "panel-title",
    state.view === "notifications" ? "通知インボックス" : "設定",
  );
  title.setAttribute("data-tauri-drag-region", "");

  const actions = create("div", "panel-actions");

  if (state.view === "notifications") {
    const refreshBtn = create("button", "icon-btn");
    refreshBtn.title = "更新";
    refreshBtn.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M1.33 1.33v4.67h4.67"/><path d="M2.34 10a6 6 0 1 0 1.16-6.52L1.33 6"/></svg>';
    refreshBtn.addEventListener("click", () => {
      void loadGroups();
    });

    const dummyBtn = create("button", "icon-btn");
    dummyBtn.title = "ダミー投入";
    dummyBtn.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 1v14"/><path d="M1 8h14"/></svg>';
    dummyBtn.addEventListener("click", async () => {
      await injectDummy();
    });

    const clearAllBtn = create("button", "icon-btn warn");
    clearAllBtn.title = "全通知をクリア";
    clearAllBtn.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h12M5.33 4V2.67a1.33 1.33 0 0 1 1.34-1.34h2.66a1.33 1.33 0 0 1 1.34 1.34V4M6.67 7.33v4M9.33 7.33v4"/><path d="M3.33 4h9.34l-.67 9.33a1.33 1.33 0 0 1-1.33 1.34H5.33A1.33 1.33 0 0 1 4 13.33L3.33 4z"/></svg>';
    clearAllBtn.addEventListener("click", async () => {
      await clearAll();
    });

    const clearAndCloseBtn = create("button", "icon-btn warn");
    clearAndCloseBtn.title = "全通知をクリアして閉じる";
    clearAndCloseBtn.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2H2v12h4"/><path d="M9 5l3 3-3 3"/><path d="M12 8H4"/></svg>';
    clearAndCloseBtn.addEventListener("click", async () => {
      const cleared = await clearAll();
      if (cleared) {
        await hideMainWindow();
      }
    });

    actions.append(refreshBtn, dummyBtn, clearAllBtn, clearAndCloseBtn);
  }

  const settingsBtn = create("button", "icon-btn");
  if (state.view === "settings") {
    settingsBtn.title = "戻る";
    settingsBtn.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10 2L4 8l6 6"/></svg>';
  } else {
    settingsBtn.title = "設定";
    settingsBtn.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="2.5"/><path d="M13.54 9.98a1.13 1.13 0 0 0 .23 1.24l.04.04a1.37 1.37 0 1 1-1.94 1.94l-.04-.04a1.13 1.13 0 0 0-1.24-.23 1.13 1.13 0 0 0-.69 1.04v.12a1.37 1.37 0 1 1-2.74 0v-.06a1.13 1.13 0 0 0-.74-1.04 1.13 1.13 0 0 0-1.24.23l-.04.04a1.37 1.37 0 1 1-1.94-1.94l.04-.04a1.13 1.13 0 0 0 .23-1.24 1.13 1.13 0 0 0-1.04-.69h-.12a1.37 1.37 0 0 1 0-2.74h.06a1.13 1.13 0 0 0 1.04-.74 1.13 1.13 0 0 0-.23-1.24l-.04-.04A1.37 1.37 0 1 1 5.08 2.83l.04.04a1.13 1.13 0 0 0 1.24.23h.05a1.13 1.13 0 0 0 .69-1.04v-.12a1.37 1.37 0 0 1 2.74 0v.06a1.13 1.13 0 0 0 .69 1.04 1.13 1.13 0 0 0 1.24-.23l.04-.04a1.37 1.37 0 1 1 1.94 1.94l-.04.04a1.13 1.13 0 0 0-.23 1.24v.05a1.13 1.13 0 0 0 1.04.69h.12a1.37 1.37 0 0 1 0 2.74h-.06a1.13 1.13 0 0 0-1.04.69z"/></svg>';
  }
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
      const groupTitleWrap = create("div", "group-title-wrap");
      if (group.iconBase64) {
        const icon = document.createElement("img");
        icon.className = "group-icon";
        icon.src = `data:image/png;base64,${group.iconBase64}`;
        icon.alt = group.appName;
        groupTitleWrap.append(icon);
      }
      const groupTitle = create(
        "h2",
        "group-title",
        `${group.appName} (${group.notifications.length})`,
      );
      groupTitleWrap.append(groupTitle);

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
      groupHeader.append(groupTitleWrap, groupActions);

      const cards = create("div", "cards");
      let cardIdx = 0;
      for (const notification of group.notifications) {
        const card = renderCard(notification);
        card.style.animationDelay = `${groupIdx * 0.06 + cardIdx * 0.03}s`;
        cards.append(card);
        cardIdx++;
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
  const time = create("span", "card-time", formatRelativeTime(notification.timestamp));
  time.dataset.timestamp = String(notification.timestamp);

  openBtn.append(label, time, summary, sub);

  const openAppBtn = create("button", "card-clear");
  openAppBtn.type = "button";
  openAppBtn.title = `アプリを開く: ${notification.bundleId}`;
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

  const cardActions = create("div", "card-actions");
  cardActions.append(openAppBtn, clearBtn);

  card.append(bar, openBtn, cardActions);
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

  const reasonTitle = create("p", "card-sub", "AI判定理由");
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

  const addBtn = create("button", "icon-btn");
  addBtn.title = "プロンプトを追加";
  addBtn.innerHTML =
    '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 1v14"/><path d="M1 8h14"/></svg>';
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
    editBtn.innerHTML =
      '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M11.33 2a1.89 1.89 0 0 1 2.67 2.67L5.33 13.33 1.33 14.67l1.34-4L11.33 2z"/></svg>';
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
    const unignoreBtn = create("button", "group-clear-btn");
    unignoreBtn.title = "無視を解除";
    unignoreBtn.innerHTML =
      '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M1 1l14 14"/><path d="M9.5 4a6 6 0 0 1 5.17 8.94"/><path d="M1.33 7.06A6 6 0 0 0 6.5 16"/><circle cx="8" cy="8" r="6"/></svg>';
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
  const saveBtn = create("button", "icon-btn");
  saveBtn.title = "保存";
  saveBtn.innerHTML =
    '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 8.5l3.5 3.5 7.5-8"/></svg>';
  saveBtn.addEventListener("click", async () => {
    if (!editing.bundleId.trim() || !editing.context.trim()) {
      state.error = "Bundle ID とコンテキストは必須です。";
      render();
      return;
    }
    await savePrompt(editing.bundleId.trim(), editing.context.trim());
    state.editingPrompt = null;
  });

  const cancelBtn = create("button", "icon-btn");
  cancelBtn.title = "キャンセル";
  cancelBtn.innerHTML =
    '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 3l10 10"/><path d="M13 3L3 13"/></svg>';
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

async function clearAll(): Promise<boolean> {
  try {
    state.error = "";
    await invokeCommand<number>("clear_all_notifications");
    await loadGroups();
    return true;
  } catch (error) {
    state.error = (error as Error).message;
    render();
    return false;
  }
}

async function hideMainWindow(): Promise<void> {
  try {
    await invokeCommand<void>("hide_main_window");
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

  const onActivate = () => {
    resetUiToMainViewAndRender();
  };

  window.addEventListener("focus", onActivate);
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) {
      onActivate();
    }
  });
}

void setupEventListener();
void loadGroups();

setInterval(() => {
  for (const el of document.querySelectorAll<HTMLElement>(".card-time")) {
    const ts = Number(el.dataset.timestamp);
    if (ts) {
      el.textContent = formatRelativeTime(ts);
    }
  }
}, 30_000);

export {};
