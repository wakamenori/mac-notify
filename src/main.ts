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
} = {
  groups: [],
  selected: null,
  error: "",
  loading: false,
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
  return `background:${color}`;
}

function render(): void {
  root.replaceChildren();

  const panel = create("main", "panel");
  const header = create("header", "panel-header");
  const title = create("h1", "panel-title", "通知インボックス");

  const actions = create("div", "panel-actions");
  const refreshBtn = create("button", "btn secondary", "更新");
  refreshBtn.addEventListener("click", () => {
    void loadGroups();
  });

  const summarizeBtn = create("button", "btn secondary", "要約");
  summarizeBtn.addEventListener("click", async () => {
    try {
      const summary = await invokeCommand<string>("summarize_notifications");
      window.alert(summary);
    } catch (error) {
      state.error = (error as Error).message;
      render();
    }
  });

  const dummyBtn = create("button", "btn secondary", "ダミー投入");
  dummyBtn.addEventListener("click", async () => {
    await injectDummy();
  });

  const clearAllBtn = create("button", "btn warn", "全通知をクリア");
  clearAllBtn.addEventListener("click", async () => {
    const ok = window.confirm("全通知をクリアしますか？");
    if (!ok) {
      return;
    }
    await clearAll();
  });

  actions.append(refreshBtn, summarizeBtn, dummyBtn, clearAllBtn);
  header.append(title, actions);

  const groups = create("section", "groups");
  if (!state.loading && state.groups.length === 0) {
    groups.append(create("div", "empty", "現在表示できる通知はありません。"));
  }

  for (const group of state.groups) {
    const section = create("section", "group");
    const groupHeader = create("div", "group-header");
    const groupTitle = create(
      "h2",
      "group-title",
      `${group.appName} (${group.notifications.length + group.hiddenCount})`,
    );

    const clearAppBtn = create("button", "group-clear-btn");
    clearAppBtn.title = "このアプリをクリア";
    clearAppBtn.innerHTML =
      '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h12M5.33 4V2.67a1.33 1.33 0 0 1 1.34-1.34h2.66a1.33 1.33 0 0 1 1.34 1.34V4M6.67 7.33v4M9.33 7.33v4"/><path d="M3.33 4h9.34l-.67 9.33a1.33 1.33 0 0 1-1.33 1.34H5.33A1.33 1.33 0 0 1 4 13.33L3.33 4z"/></svg>';
    clearAppBtn.addEventListener("click", async () => {
      await clearApp(group.bundleId);
    });

    groupHeader.append(groupTitle, clearAppBtn);

    const cards = create("div", "cards");
    for (const notification of group.notifications) {
      cards.append(renderCard(notification));
    }

    if (group.hiddenCount > 0) {
      cards.append(
        create("p", "hidden-row", `他 ${group.hiddenCount} 件は省略されています`),
      );
    }

    section.append(groupHeader, cards);
    groups.append(section);
  }

  panel.append(header, groups);

  if (state.error) {
    panel.append(create("p", "error", state.error));
  }

  root.append(panel);

  if (state.selected) {
    root.append(renderDialog(state.selected));
  }
}

function renderCard(notification: UiNotification): HTMLElement {
  const card = create("article", "card");

  const bar = create("div", "card-bar");
  bar.style.background = notification.urgencyColor;

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

  const clearBtn = create("button", "card-clear", "×");
  clearBtn.type = "button";
  clearBtn.title = "この通知をクリア";
  clearBtn.addEventListener("click", async (event) => {
    event.stopPropagation();
    await clearOne(notification.id);
  });

  card.append(bar, openBtn, clearBtn);
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

  const clearBtn = create("button", "dialog-icon-btn warn");
  clearBtn.innerHTML =
    '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4h12M5.33 4V2.67a1.33 1.33 0 0 1 1.34-1.34h2.66a1.33 1.33 0 0 1 1.34 1.34V4M6.67 7.33v4M9.33 7.33v4"/><path d="M3.33 4h9.34l-.67 9.33a1.33 1.33 0 0 1-1.33 1.34H5.33A1.33 1.33 0 0 1 4 13.33L3.33 4z"/></svg>';
  clearBtn.title = "この通知をクリア";
  clearBtn.addEventListener("click", async () => {
    await clearOne(notification.id);
    state.selected = null;
    render();
  });

  actions.append(closeBtn, clearBtn);
  dialog.append(title, meta, reasonTitle, reason, originalTitle, original, actions);
  overlay.append(dialog);
  return overlay;
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
