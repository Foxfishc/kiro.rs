const API_KEY_STORAGE_KEY = 'adminApiKey'
const THEME_STORAGE_KEY = 'adminTheme'

export type ThemePref = 'light' | 'dark'

export const storage = {
  getApiKey: () => localStorage.getItem(API_KEY_STORAGE_KEY),
  setApiKey: (key: string) => localStorage.setItem(API_KEY_STORAGE_KEY, key),
  removeApiKey: () => localStorage.removeItem(API_KEY_STORAGE_KEY),

  getTheme: (): ThemePref => {
    const saved = localStorage.getItem(THEME_STORAGE_KEY)
    if (saved === 'light' || saved === 'dark') return saved
    // 未设置时跟随系统偏好
    if (typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches) {
      return 'dark'
    }
    return 'light'
  },
  setTheme: (theme: ThemePref) => localStorage.setItem(THEME_STORAGE_KEY, theme),
}
