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
