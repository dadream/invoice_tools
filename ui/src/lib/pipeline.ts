export function linkOnlyEmailNotice(count: number): string {
  const safeCount = Number.isSafeInteger(count) && count > 0 ? count : 0;
  return `另发现 ${safeCount} 封疑似通过邮件正文链接交付的发票。出于安全原因，软件不会自动打开或下载正文链接。请先在邮箱客户端核对发件人和链接域名，手动下载发票文件，再运行新流水线并选择“本地文件”。`;
}

export interface MonthDateRange {
  start: string;
  end: string;
}

export function currentLocalMonth(now = new Date()): string {
  const year = now.getFullYear();
  const month = String(now.getMonth() + 1).padStart(2, '0');
  return `${year}-${month}`;
}

export function monthDateRange(month: string): MonthDateRange | null {
  const match = /^(\d{4})-(\d{2})$/.exec(month);
  if (!match) return null;

  const year = Number(match[1]);
  const monthNumber = Number(match[2]);
  if (year < 1 || monthNumber < 1 || monthNumber > 12) return null;

  const lastDay = new Date(Date.UTC(year, monthNumber, 0)).getUTCDate();
  return {
    start: `${month}-01`,
    end: `${month}-${String(lastDay).padStart(2, '0')}`
  };
}
