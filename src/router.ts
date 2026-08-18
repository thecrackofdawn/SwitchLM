import { createRouter, createWebHashHistory, type RouteRecordRaw } from "vue-router";

// Hash history suits Tauri's asset loading (no server-side routing).
export const routes: RouteRecordRaw[] = [
  { path: "/", redirect: "/dashboard" },
  {
    path: "/dashboard",
    name: "dashboard",
    component: () => import("./views/Dashboard.vue"),
    meta: { title: "概览" },
  },
  {
    path: "/sources",
    name: "sources",
    component: () => import("./views/Sources.vue"),
    meta: { title: "服务商" },
  },
  {
    path: "/profiles",
    name: "profiles",
    component: () => import("./views/Profiles.vue"),
    meta: { title: "路由" },
  },
  {
    path: "/fallback",
    name: "fallback",
    component: () => import("./views/Fallback.vue"),
    meta: { title: "故障转移" },
  },
  {
    path: "/usage",
    name: "usage",
    component: () => import("./views/Usage.vue"),
    meta: { title: "套餐用量" },
  },
  {
    path: "/settings",
    name: "settings",
    component: () => import("./views/Settings.vue"),
    meta: { title: "设置" },
  },
];

export const router = createRouter({
  history: createWebHashHistory(),
  routes,
});

const LAST_ROUTE_KEY = "switchlm:lastRoute";

// 记录最后访问的路由：webview 被后台销毁后重建时，SPA 重新加载，据此回到用户离开的页面。
router.afterEach((to) => {
  try {
    localStorage.setItem(LAST_ROUTE_KEY, to.path);
  } catch {
    // localStorage 不可用时静默放弃（不影响核心功能）。
  }
});

// 启动时恢复上次路由（仅当存在且能解析到真实路由，否则保持默认 /dashboard）。
export function restoreLastRoute() {
  try {
    const last = localStorage.getItem(LAST_ROUTE_KEY);
    if (last && router.resolve(last).matched.length > 0) {
      router.replace(last).catch(() => { /* 忽略懒加载分片失败：恢复失败则保持默认 /dashboard */ });
    }
  } catch {
    // 忽略：默认停在 /dashboard。
  }
}
