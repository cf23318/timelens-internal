import { Component, type ReactNode } from "react";
import i18n from "../i18n";

/** Render failures have a local recovery path and never start or stop background collection. */
export class DesktopPageBoundary extends Component<{ children: ReactNode; resetKey?: string }, { error: string | null }> {
  state: { error: string | null } = { error: null };
  static getDerivedStateFromError(error: unknown) { return { error: error instanceof Error ? error.message : String(error) }; }
  componentDidUpdate(previous: { resetKey?: string }) {
    if (previous.resetKey !== this.props.resetKey && this.state.error !== null) this.setState({ error: null });
  }
  render() {
    if (this.state.error === null) return this.props.children;
    const english = i18n.language.startsWith("en");
    return <div className="desktop-page" role="alert"><section className="desktop-card desktop-empty"><h2>{english ? "This view could not be displayed" : "页面暂时无法显示"}</h2><p>{english ? "Retry this view or return to the task page. Background collection does not change automatically; pause it from the toolbar or tray if needed." : "可以重试显示或返回采集任务。后台采集状态不会自动改变，需要暂停时可使用顶栏或托盘菜单。"}</p><div className="flex justify-center gap-3"><button className="desktop-button" onClick={() => this.setState({error:null})}>{english ? "Retry this view" : "重试显示"}</button><a className="desktop-button desktop-button-primary" href="/task">{english ? "Return to task" : "返回采集任务"}</a></div><details className="desktop-details mt-6 text-left"><summary>{english ? "Error details" : "错误详情"}</summary><p className="break-words">{this.state.error}</p></details></section></div>;
  }
}
