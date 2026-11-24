# MiniTranslation

迷你 AI 翻译软件：常驻系统托盘，一个热键呼出，中文与英语（或日、韩、法、德、西、俄语）互译。

翻译由大模型完成，支持任何 OpenAI 接口格式兼容的服务（OpenAI / DeepSeek / Kimi / Ollama 本地模型等）。可配置多个接口，按顺序使用，失败时自动切换。

## 使用

- `Alt+Q`：显示 / 隐藏窗口，可在设置中改为其他组合键、双击 Ctrl / Alt / Shift 或双击鼠标中键
- `回车`：翻译（自动判断方向），`Shift+回车` 换行
- `Tab`：朗读外文
- `Esc`：隐藏窗口

首次使用请在托盘图标右键菜单打开“设置”，填写 API 地址、API Key 和模型，例如：

| 服务 | API 地址 | 模型示例 |
| --- | --- | --- |
| DeepSeek | `https://api.deepseek.com/v1` | `deepseek-chat` |
| OpenAI | `https://api.openai.com/v1` | `gpt-4o-mini` |
| Ollama（本地） | `http://localhost:11434/v1` | `qwen3` |

配置保存在 `%AppData%\MiniTranslation\settings.json`，错误日志在 `%LocalAppData%\MiniTranslation\minits.log`。

## 下载

从 [Releases](../../releases) 下载：

- `MiniTranslation-Setup.exe`：安装包（可勾选开机自启）
- `MiniTranslation-win-x64.zip`：绿色版，解压即用

## 构建

需要 Rust stable 与 MSVC 工具链（Windows 10 及以上）：

```bash
cargo build --release
```

产物为 `target/release/MiniTranslation.exe`。推送与 `Cargo.toml` 版本一致的 `v*` tag（如 `v3.0.0`）会自动构建 zip 与安装包并发布 GitHub Release。
