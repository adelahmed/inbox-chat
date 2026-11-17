//! In-memory sample mailbox for tests and `--demo` (try the app without Outlook), and the sample
//! user's profile. Everyone in it is fictional.

use crate::mail::*;
use crate::text::{clean_text, insert_styled, strip_quoted, truncate_chars};
use chrono::{Duration, Local, NaiveDateTime};
use std::sync::Mutex;

#[derive(Clone)]
pub struct FakeItem {
    pub entry_id: String,
    pub subject: String,
    pub sender: String,
    pub email: String,
    pub body: String,
    pub html: String,
    pub received: Option<NaiveDateTime>,
    pub modified: NaiveDateTime,
    pub unread: bool,
    pub to: String,
    pub cc: String,
    pub folder: String,
    pub conv: Option<u32>,
    pub attachments: Vec<String>,
    /// Outlook item class: 43 mail, 53 meeting invitation.
    pub class: i64,
    /// Outlook's last action (102 replied, 103 replied all, 104 forwarded) and when.
    pub last_verb: Option<(i64, String)>,
}

pub struct FakeMail {
    pub state: Mutex<Vec<FakeItem>>,
    next: Mutex<u64>,
}

const FOLDERS: [&str; 5] = ["Inbox", "Inbox/Projects", "Sent Items", "Drafts", "Deleted Items"];

/// Address book used to resolve recipients: two "Rania"s make that name ambiguous.
const DIRECTORY: [(&str, &str); 5] = [
    ("Rania Saeed", "rania.s@example.com"),
    ("Rania Khoury", "rania.k@example.com"),
    ("Mona Al-Khatib", "mona@example.com"),
    ("Hassan Alattar", "hassan.attar@example.com"),
    ("Sara Al-Masri", "sara@example.com"),
];

/// Like Outlook's Resolve(): exact name/address first, then a unique partial name match. Any SMTP
/// address resolves (as Outlook does for one-off addresses).
fn resolve(name: &str) -> Option<(String, String)> {
    let key = name.trim().to_lowercase();
    let exact: Vec<_> = DIRECTORY.iter().filter(|d| d.0.to_lowercase() == key || d.1 == key).collect();
    let hits: Vec<_> = if exact.is_empty() { DIRECTORY.iter().filter(|d| d.0.to_lowercase().contains(&key)).collect() } else { exact };
    match hits.as_slice() {
        [one] => Some((one.0.to_string(), one.1.to_string())),
        _ if key.contains('@') => Some((key.clone(), key)),
        _ => None,
    }
}

/// Address for a sample person: the directory's, else first-name@example.com.
fn email_of(name: &str) -> String {
    DIRECTORY.iter().find(|d| d.0 == name).map(|d| d.1.to_string())
        .unwrap_or_else(|| format!("{}@example.com", name.split_whitespace().next().unwrap_or("x").to_lowercase()))
}

const DEMO_PDF: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 420 200]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n\
4 0 obj<</Length 58>>stream\nBT /F1 22 Tf 40 110 Td (Q3 review pack - sample PDF) Tj ET\nendstream endobj\n\
5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";

/// An email as Outlook/Word writes it: a `<style>` block, class-based paragraphs, inline styles,
/// a bordered table with shaded header cells, colours, highlight and a list. The reading pane must show
/// all of it (it relies on the email's own CSS, which a too-strict page policy silently drops).
const DEMO_RICH_HTML: &str = r##"<html xmlns:o="urn:schemas-microsoft-com:office:office"><head>
<meta http-equiv=Content-Type content="text/html; charset=utf-8">
<style><!--
p.MsoNormal, li.MsoNormal {margin:0cm; font-size:11.0pt; font-family:"Calibri",sans-serif;}
table.MsoTableGrid {border-collapse:collapse;}
.Accent {color:#2F5496; font-weight:bold;}
--></style></head>
<body lang=EN-US link="#0563C1" style='word-wrap:break-word'><div class=WordSection1>
<p class=MsoNormal>Dear Adel,</p><p class=MsoNormal>&nbsp;</p>
<p class=MsoNormal><span class=Accent style='font-size:14.0pt'>Weekly KPI summary – week 46</span></p>
<p class=MsoNormal>Figures below are <b>provisional</b>; the ones in <span style='color:#C00000'>red</span> missed the target.
Please <span style='background:yellow'>confirm the delivery figures by Thursday</span>.</p><p class=MsoNormal>&nbsp;</p>
<table class=MsoTableGrid border=1 cellspacing=0 cellpadding=0 style='border-collapse:collapse;border:none'>
<tr><td width=180 style='width:135pt;border:solid windowtext 1.0pt;background:#D9E2F3;padding:0cm 5.4pt'><p class=MsoNormal><b>Measure</b></p></td>
<td width=90 style='width:67.5pt;border:solid windowtext 1.0pt;border-left:none;background:#D9E2F3;padding:0cm 5.4pt'><p class=MsoNormal align=right style='text-align:right'><b>Target</b></p></td>
<td width=90 style='width:67.5pt;border:solid windowtext 1.0pt;border-left:none;background:#D9E2F3;padding:0cm 5.4pt'><p class=MsoNormal align=right style='text-align:right'><b>Actual</b></p></td></tr>
<tr><td style='border:solid windowtext 1.0pt;border-top:none;padding:0cm 5.4pt'><p class=MsoNormal>On-time delivery</p></td>
<td style='border-top:none;border-left:none;border-bottom:solid windowtext 1.0pt;border-right:solid windowtext 1.0pt;padding:0cm 5.4pt'><p class=MsoNormal align=right style='text-align:right'>95%</p></td>
<td style='border-top:none;border-left:none;border-bottom:solid windowtext 1.0pt;border-right:solid windowtext 1.0pt;padding:0cm 5.4pt'><p class=MsoNormal align=right style='text-align:right'><span style='color:#C00000'>91%</span></p></td></tr>
<tr><td style='border:solid windowtext 1.0pt;border-top:none;padding:0cm 5.4pt'><p class=MsoNormal>Orders processed</p></td>
<td style='border-top:none;border-left:none;border-bottom:solid windowtext 1.0pt;border-right:solid windowtext 1.0pt;padding:0cm 5.4pt'><p class=MsoNormal align=right style='text-align:right'>12,000</p></td>
<td style='border-top:none;border-left:none;border-bottom:solid windowtext 1.0pt;border-right:solid windowtext 1.0pt;padding:0cm 5.4pt'><p class=MsoNormal align=right style='text-align:right'>12,450</p></td></tr>
</table>
<p class=MsoNormal>&nbsp;</p>
<p class=MsoNormal dir=RTL style='text-align:right;direction:rtl'><span lang=AR-SA style='font-family:"Sakkal Majalla";font-size:14.0pt'>ملاحظات الفريق:</span></p>
<ul style='margin-top:0cm' type=disc>
<li class=MsoNormal>A second courier starts next week.</li>
<li class=MsoNormal><i>Stock count starts on Monday.</i></li>
</ul>
<p class=MsoNormal>&nbsp;</p><p class=MsoNormal>Regards,<br><b>Sara</b></p></div></body></html>"##;

/// The sample user's profile (fictional): Arabic greeting and sign-off applied by code, a special
/// greeting for one person, department keywords. Used by `--demo` and the tests.
pub fn sample_profile() -> crate::profile::Profile {
    use crate::profile::{Profile, SpecialAddressee, Style};
    Profile {
        my_mentions: vec!["د. عادل".into(), "Dr. Adel".into()],
        job_title: "Head of Customer Experience".into(),
        dept_name: "Customer Experience".into(),
        dept_keywords: vec!["الخدمات المتميزة".into(), "Customer experience".into(), "تجربة العملاء".into()],
        background: "The Customer Experience department (الخدمات المتميزة) runs customer research, service quality and the support centre.".into(),
        language: "Arabic".into(),
        style: Style {
            enabled: true,
            greeting_single: "مع التحية والتقدير ل{name}".into(),
            greeting_multi: "مع التحية والتقدير للجميع".into(),
            special: vec![SpecialAddressee {
                matches: "طارق الناصر|طارق بن عمر الناصر|Tariq Alnasser|Tariq Al-Nasser".into(),
                greeting: "مع التحية والتقدير لسعادة الرئيس التنفيذي".into(),
            }],
            signoff: "مع تحياتي\n-عادل".into(),
            font_family: "Sakkal Majalla".into(),
            font_size_pt: 14.0,
            rtl: true,
        },
        ..Profile::default()
    }
}

fn iso(d: Option<NaiveDateTime>) -> Option<String> {
    d.map(|d| d.format("%Y-%m-%dT%H:%M").to_string())
}

impl FakeMail {
    pub fn seeded() -> Self {
        let fm = FakeMail { state: Mutex::new(Vec::new()), next: Mutex::new(1) };
        let now = Local::now().naive_local();
        fm.add("Inbox", "Data lake migration - cutover date", "Sara Al-Masri", "sara@example.com",
            "Hi Adel,\nCan we confirm the Iceberg cutover for 30 November? I need your approval by Tuesday.\nThanks,\nSara",
            now - Duration::days(2), false, Some(1), &[]);
        fm.add("Sent Items", "RE: Data lake migration - cutover date", "Adel Ahmed", "adel@example.com",
            "Sara, checking with infra.\n\n________________________________\nFrom: Sara Al-Masri\nSent: Friday\nold quoted stuff",
            now - Duration::hours(44), false, Some(1), &[]);
        fm.add("Inbox", "RE: Data lake migration - cutover date", "Sara Al-Masri", "sara@example.com",
            "Any update? Infra says the 30th works for them.\n\nFrom: Adel Ahmed\nSent: Saturday\nquoted",
            now - Duration::hours(3), true, Some(1), &[]);
        fm.add("Inbox", "طلب اجتماع بخصوص ميزانية ٢٠٢٧", "Khalid Mansour", "khalid@example.com",
            "السلام عليكم د. عادل،\nنرغب في عقد اجتماع يوم الأربعاء لمناقشة الميزانية.\nمع التحية",
            now - Duration::hours(5), true, None, &["budget_2027.xlsx"]);
        fm.add("Inbox", "Old newsletter", "News", "news@vendor.com", "Nothing important",
            now - Duration::days(40), false, None, &[]);
        {
            // Outlook remembers that the user replied to Sara's first email (the reply is in Sent Items).
            let mut st = fm.state.lock().unwrap();
            let replied_at = iso(Some(now - Duration::hours(44))).unwrap();
            if let Some(m) = st.iter_mut().find(|m| m.subject == "Data lake migration - cutover date") {
                m.last_verb = Some((102, replied_at));
            }
            if let Some(m) = st.iter_mut().find(|m| m.folder == "Sent Items") {
                m.to = "Sara Al-Masri".into();
            }
        }
        fm
    }

    /// The test mailbox plus demo-only emails (outside the tests' 7-day window so test counts hold).
    pub fn demo() -> Self {
        let fm = Self::seeded();
        let now = Local::now().naive_local();
        fm.add("Inbox", "Q3 product launch – final review pack", "Noura Aziz", "noura@example.com",
            "Dear Adel,\n\nPlease find attached the review pack for the Q3 launch and the pricing note.\n\
             The dashboard is live here: https://example.org/q3\n\nKind regards,\nNoura",
            now - Duration::days(9), false, None, &["Q3_review_pack.pdf", "pricing_notes.txt"]);
        fm.add("Inbox/Projects", "Website redesign working group – agenda", "Omar Saleh", "omar@example.com",
            "Hi all,\n\nAgenda for Thursday:\n1. Site map\n2. Design options\n3. Next steps",
            now - Duration::days(12), true, None, &["agenda.docx"]);
        fm.add("Inbox", "Weekly KPI summary – week 46", "Sara Al-Masri", "sara@example.com",
            "Dear Adel,\n\nWeekly KPI summary – week 46\nFigures below are provisional. Please confirm the delivery figures by Thursday.\n\n\
             Measure\tTarget\tActual\nOn-time delivery\t95%\t91%\nOrders processed\t12,000\t12,450\n\nRegards,\nSara",
            now - Duration::days(8), false, None, &[]);
        if let Some(m) = fm.state.lock().unwrap().last_mut() {
            m.html = DEMO_RICH_HTML.into();
        }
        // Past replies in the user's style, for learning (Sent Items, 15–60 days old).
        let past: [(&str, &str, &str, &str, &str); 12] = [
            ("Khalid Mansour", "خالد", "طلب بيانات المبيعات", "نرجو تزويدنا ببيانات المبيعات الربعية لاستكمال التقرير.", "تم، سيتم إرسال البيانات يوم الأحد بإذن الله."),
            ("Sara Al-Masri", "سارة", "Data lake access", "Could you approve access for two analysts?", "تمت الموافقة، ونأمل التنسيق مع فريق البنية التحتية لتفعيل الصلاحيات."),
            ("Omar Saleh", "عمر", "اجتماع مجموعة العمل", "هل يناسبكم الاجتماع يوم الخميس؟", "يناسبني الخميس الساعة العاشرة صباحاً."),
            ("Noura Aziz", "نورة", "مراجعة حزمة الإصدار", "نأمل مراجعة حزمة الإصدار قبل نهاية الأسبوع.", "شكراً لكِ، سأراجعها وأوافيكِ بالملاحظات يوم الأربعاء."),
            ("Khalid Mansour", "خالد", "الميزانية التقديرية", "نرجو إفادتنا بالميزانية التقديرية لمشروع التطبيق الجديد.", "سيتم إرسال التقدير بعد اعتماده من الإدارة، المتوقع خلال أسبوع."),
            ("Hassan Alattar", "حسن", "خطة المشروع", "Can you share the project plan for the new app?", "مرفق خطة المشروع، ويسعدنا تلقي ملاحظاتكم."),
            ("Mona Al-Khatib", "منى", "ورشة تجربة العملاء", "نود مشاركتكم في ورشة تجربة العملاء.", "يسعدنا المشاركة، ونأمل إرسال جدول الورشة."),
            ("Sara Al-Masri", "سارة", "رضا العملاء", "هل لديكم ملاحظات على تقرير رضا العملاء؟", "لا ملاحظات جوهرية، ونقترح إضافة مقارنة بالربع السابق."),
            ("Omar Saleh", "عمر", "Dashboard review", "Please review the KPI dashboard draft.", "تمت المراجعة، والملاحظات مدرجة في الملف المرفق."),
            ("Noura Aziz", "نورة", "طلب دعم فني", "نحتاج دعماً فنياً لمشروع الموقع الإلكتروني.", "تم توجيه الفريق لدعمكم، وسيتواصل معكم أ. حسن."),
            ("Khalid Mansour", "خالد", "موعد الاجتماع", "هل يمكن تأجيل الاجتماع إلى الأسبوع القادم؟", "لا مانع، نقترح يوم الاثنين."),
            ("Hassan Alattar", "حسن", "صلاحيات المنصة", "نرجو اعتماد صلاحيات المنصة للموظفين الجدد.", "تم الاعتماد."),
        ];
        // Requests the user sent: Mona hasn't answered (open), Hassan has (closed); a thank-you isn't a request.
        let sent = |to: &str, subject: &str, body: &str, ago: Duration| {
            fm.add("Sent Items", subject, "Adel Ahmed", "adel@example.com", body, now - ago, false, None, &[]);
            if let Some(m) = fm.state.lock().unwrap().last_mut() {
                m.to = to.to_string();
            }
        };
        sent("Mona Al-Khatib", "دراسة عرض الشريك التقني",
             "مع التحية والتقدير لمنى\n\nنأمل دراسة العرض المرفق والرد على الجهة قبل الخميس.\n\nمع تحياتي\n-عادل", Duration::days(3));
        sent("Hassan Alattar", "تجهيز تقرير المبيعات",
             "مع التحية والتقدير لحسن\n\nيرجى تجهيز تقرير المبيعات للربع الثالث.\n\nمع تحياتي\n-عادل", Duration::days(5));
        sent("Omar Saleh", "شكر وتقدير", "مع التحية والتقدير لعمر\n\nشكراً لكم.\n\nمع تحياتي\n-عادل", Duration::days(1));
        fm.add("Inbox", "RE: تجهيز تقرير المبيعات", "Hassan Alattar", "hassan.attar@example.com",
               "تم تجهيز التقرير وهو مرفق.\n\nFrom: Adel Ahmed\nSent: Tuesday\nيرجى تجهيز تقرير المبيعات", now - Duration::days(2),
               false, None, &[]);
        // A meeting invitation (forwarded as an email with the invitation attached).
        fm.add("Inbox", "ورشة تخطيط المشاريع — المستندات الداعمة", "Omar Saleh", "omar@example.com",
               "يسعدنا دعوتكم لورشة تخطيط المشاريع يوم الأربعاء الساعة العاشرة.", now - Duration::hours(30), false, None, &[]);
        if let Some(m) = fm.state.lock().unwrap().last_mut() {
            m.class = 53;
        }
        // Two different Ranias the user has written to: a bare "Rania" must stay ambiguous.
        for (i, who) in ["Rania Saeed", "Rania Khoury"].iter().enumerate() {
            fm.add("Sent Items", "ملاحظات", "Adel Ahmed", "adel@example.com", "شكراً لكم.", now - Duration::days(30 + i as i64), false, None, &[]);
            if let Some(m) = fm.state.lock().unwrap().last_mut() {
                m.to = who.to_string();
            }
        }
        for (i, (from, ar, subj, incoming, reply)) in past.iter().enumerate() {
            let body = format!("مع التحية والتقدير ل{ar}\n\n{reply}\n\nمع تحياتي\n-عادل\n\nFrom: {from}\nSent: Sunday\nTo: Adel Ahmed\nSubject: {subj}\n\n{incoming}");
            fm.add("Sent Items", &format!("RE: {subj}"), "Adel Ahmed", "adel@example.com", &body,
                   now - Duration::days(15 + 4 * i as i64), false, None, &[]);
            if let Some(m) = fm.state.lock().unwrap().last_mut() {
                m.to = from.to_string();
            }
        }
        {
            let mut st = fm.state.lock().unwrap();
            if let Some(m) = st.iter_mut().find(|m| m.subject.starts_with("Q3 product")) {
                m.html = "<html><body><p>Dear Adel,</p><p>Please find attached the review pack for the Q3 launch \
                          and the pricing note.</p><p>The dashboard is live <a href=\"https://example.org/q3\">here</a>.</p>\
                          <p><img src=\"https://tracker.example.com/pixel.png\" width=\"1\" height=\"1\"></p>\
                          <p>Kind regards,<br><b>Noura</b></p></body></html>".into();
            }
        }
        fm
    }

    fn new_id(&self) -> String {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        format!("{:0140X}", *n)
    }

    #[allow(clippy::too_many_arguments)]
    fn add(&self, folder: &str, subject: &str, sender: &str, email: &str, body: &str, received: NaiveDateTime,
           unread: bool, conv: Option<u32>, attachments: &[&str]) {
        let item = FakeItem {
            entry_id: self.new_id(),
            subject: subject.into(),
            sender: sender.into(),
            email: email.into(),
            body: body.into(),
            html: format!("<html><head></head><body><p>{}</p></body></html>",
                          crate::text::escape_html(body).replace('\n', "<br>")),
            received: Some(received),
            modified: received,
            unread,
            to: "Adel Ahmed".into(),
            cc: String::new(),
            folder: folder.into(),
            conv,
            attachments: attachments.iter().map(|s| s.to_string()).collect(),
            class: 43,
            last_verb: None,
        };
        self.state.lock().unwrap().push(item);
    }

    pub fn drafts(&self) -> Vec<FakeItem> {
        self.state.lock().unwrap().iter().filter(|m| m.folder == "Drafts").cloned().collect()
    }

    fn resolve(&self, path: &str) -> MailResult<String> {
        let parts: Vec<&str> = path.trim().trim_matches(['/', '\\']).split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        let parts = if parts.is_empty() { vec!["inbox"] } else { parts };
        let mut cur = match parts[0].to_lowercase().as_str() {
            "inbox" => "Inbox".to_string(),
            "sent" | "sent items" => "Sent Items".to_string(),
            "drafts" => "Drafts".to_string(),
            "deleted" | "deleted items" => "Deleted Items".to_string(),
            other => {
                return FOLDERS.iter().find(|f| f.to_lowercase() == other).map(|f| f.to_string())
                    .ok_or_else(|| MailError::NotFound(format!("Folder '{}' not found.", parts[0])));
            }
        };
        for name in &parts[1..] {
            let children: Vec<&str> = FOLDERS.iter().filter_map(|f| f.strip_prefix(&format!("{cur}/"))).collect();
            match children.iter().find(|c| c.eq_ignore_ascii_case(name)) {
                Some(c) => cur = format!("{cur}/{c}"),
                None => {
                    let leaf = cur.rsplit('/').next().unwrap_or(&cur).to_string();
                    let avail = if children.is_empty() { "(none)".to_string() } else { children.join(", ") };
                    return Err(MailError::NotFound(format!("Folder '{name}' not found under '{leaf}'. Available: {avail}")));
                }
            }
        }
        Ok(cur)
    }

    fn summary(m: &FakeItem) -> Summary {
        Summary {
            entry_id: m.entry_id.clone(),
            kind: if m.class == 53 { "meeting" } else { "email" }.into(),
            subject: if m.subject.is_empty() { "(no subject)".into() } else { m.subject.clone() },
            from_name: m.sender.clone(),
            from: m.email.clone(),
            received: iso(m.received),
            unread: m.unread,
            importance: "normal".into(),
            attachments: m.attachments.len() as i64,
            preview: truncate_chars(&strip_quoted(&clean_text(&m.body)), PREVIEW_CHARS),
            folder: None,
            tags: vec![],
            to: m.to.clone(),
            cc: m.cc.clone(),
            body_head: truncate_chars(&m.body, 4000),
        }
    }

    fn get(&self, entry_id: &str) -> MailResult<FakeItem> {
        self.state.lock().unwrap().iter().find(|m| m.entry_id == entry_id).cloned()
            .ok_or_else(|| MailError::NotFound("Email not found (it may have been moved or deleted).".into()))
    }

    fn save_draft(&self, subject: String, to: String, cc: String, html: String) -> SavedDraft {
        let item = FakeItem {
            entry_id: self.new_id(), subject: subject.clone(), sender: "Adel Ahmed".into(), email: "adel@example.com".into(),
            body: String::new(), html, received: None, modified: Local::now().naive_local(), unread: false,
            to: to.clone(), cc: cc.clone(), folder: "Drafts".into(), conv: None, attachments: vec![], class: 43, last_verb: None,
        };
        let id = item.entry_id.clone();
        self.state.lock().unwrap().push(item);
        SavedDraft { entry_id: id, subject, to, cc }
    }
}

impl MailStore for FakeMail {
    fn list_folders(&self, _max_depth: usize) -> MailResult<Vec<Folder>> {
        let st = self.state.lock().unwrap();
        Ok(FOLDERS.iter().map(|f| Folder {
            path: f.to_string(),
            unread: Some(st.iter().filter(|m| m.folder == *f && m.unread).count() as i64),
        }).collect())
    }

    fn list_messages(&self, folder: &str, days: Option<i64>, unread_only: bool, limit: usize) -> MailResult<Vec<Summary>> {
        let f = self.resolve(folder)?;
        let cutoff = days.map(|d| Local::now().naive_local() - Duration::days(d));
        let mut items: Vec<FakeItem> = self.state.lock().unwrap().iter()
            .filter(|m| m.folder == f && (!unread_only || m.unread)).cloned().collect();
        items.sort_by(|a, b| b.received.cmp(&a.received));
        Ok(items.iter().take_while(|m| !matches!((cutoff, m.received), (Some(c), Some(r)) if r < c))
            .take(limit).map(Self::summary).collect())
    }

    fn search(&self, q: &SearchQuery, folders: &[String], days: Option<i64>, limit: usize) -> MailResult<Vec<Summary>> {
        dasl_filter(q, None)?;
        let cutoff = days.map(|d| Local::now().naive_local() - Duration::days(d));
        let folders = if folders.is_empty() { vec!["inbox".to_string(), "sent".to_string()] } else { folders.to_vec() };
        let run = |words: Option<&[String]>| -> MailResult<Vec<Summary>> {
            let mut out = Vec::new();
            for fname in &folders {
                let f = self.resolve(fname)?;
                let st = self.state.lock().unwrap();
                for m in st.iter().filter(|m| m.folder == f) {
                    let hay = format!("{} {} {}", m.subject, m.body, m.sender).to_lowercase();
                    let q_ok = match words {
                        Some(ws) => ws.iter().all(|w| hay.contains(&w.to_lowercase())),
                        None => q.query.is_empty() || hay.contains(&q.query.to_lowercase()),
                    };
                    let s_ok = q.sender.is_empty() || format!("{} {}", m.sender, m.email).to_lowercase().contains(&q.sender.to_lowercase());
                    let r_ok = q.recipient.is_empty() || format!("{} {}", m.to, m.cc).to_lowercase().contains(&q.recipient.to_lowercase());
                    let d_ok = !matches!((cutoff, m.received), (Some(c), Some(r)) if r < c);
                    if q_ok && s_ok && r_ok && d_ok {
                        let mut s = Self::summary(m);
                        s.folder = Some(fname.clone());
                        out.push(s);
                    }
                }
            }
            out.sort_by(|a, b| b.received.cmp(&a.received));
            out.truncate(limit);
            Ok(out)
        };
        let found = run(None)?;
        let words = fallback_words(&q.query);
        if found.is_empty() && !words.is_empty() { run(Some(&words)) } else { Ok(found) }
    }

    fn later_in_conversation(&self, entry_id: &str) -> MailResult<Vec<ThreadEvent>> {
        let m = self.get(entry_id)?;
        let topic = topic_of(&m.subject);
        let st = self.state.lock().unwrap();
        let mut v: Vec<ThreadEvent> = st.iter()
            .filter(|x| x.entry_id != m.entry_id && x.folder != "Drafts" && topic_of(&x.subject) == topic)
            .filter(|x| matches!((m.received, x.received), (Some(b), Some(r)) if r > b))
            .map(|x| ThreadEvent { entry_id: x.entry_id.clone(), from_name: x.sender.clone(), from: x.email.clone(), when: iso(x.received) })
            .collect();
        v.sort_by(|a, b| a.when.cmp(&b.when));
        Ok(v)
    }

    fn related(&self, entry_id: &str) -> MailResult<Related> {
        let m = self.get(entry_id)?;
        let topic = |s: &str| {
            let mut t = s.trim().to_string();
            loop {
                let lower = t.to_lowercase();
                match ["re:", "fw:", "fwd:", "رد:", "إعادة توجيه:"].iter().find(|p| lower.starts_with(*p)) {
                    Some(p) => t = t[p.len()..].trim().to_string(),
                    None => return t,
                }
            }
        };
        let mine = topic(&m.subject);
        let st = self.state.lock().unwrap();
        let items = st.iter()
            .filter(|x| (x.folder == "Sent Items" || x.folder == "Drafts") && topic(&x.subject) == mine)
            .filter(|x| {
                let when = if x.folder == "Drafts" { Some(x.modified) } else { x.received };
                !matches!((m.received, when), (Some(s), Some(w)) if w < s)
            })
            .map(|x| RelatedItem {
                entry_id: x.entry_id.clone(), folder: if x.folder == "Drafts" { "drafts" } else { "sent" }.into(),
                kind: kind_from_subject(&x.subject).into(), subject: x.subject.clone(), to: x.to.clone(), cc: x.cc.clone(),
                when: iso(if x.folder == "Drafts" { Some(x.modified) } else { x.received }),
                preview: truncate_chars(&strip_quoted(&clean_text(&x.body)), PREVIEW_CHARS),
            }).collect();
        Ok(Related {
            last_action: m.last_verb.as_ref().and_then(|(c, _)| verb_name(*c)).map(String::from),
            last_action_at: m.last_verb.as_ref().map(|(_, t)| t.clone()),
            items,
        })
    }

    fn get_message(&self, entry_id: &str) -> MailResult<Message> {
        let m = self.get(entry_id)?;
        let body = clean_text(&m.body);
        Ok(Message {
            entry_id: m.entry_id.clone(), kind: "email".into(), subject: m.subject.clone(), from_name: m.sender.clone(),
            from: m.email.clone(), received: iso(m.received), unread: m.unread, importance: "normal".into(),
            attachments: m.attachments.len() as i64, to: m.to.clone(), cc: m.cc.clone(),
            body: truncate_chars(&body, BODY_CHARS), attachment_names: m.attachments.clone(), conversation: m.subject.clone(),
        })
    }

    fn get_thread(&self, entry_id: &str, max_messages: usize, chars: usize) -> MailResult<Vec<ThreadMsg>> {
        let item = self.get(entry_id)?;
        let mut msgs: Vec<FakeItem> = match item.conv {
            Some(c) => self.state.lock().unwrap().iter().filter(|m| m.conv == Some(c)).cloned().collect(),
            None => vec![item],
        };
        msgs.sort_by(|a, b| a.received.cmp(&b.received));
        Ok(msgs.iter().rev().take(max_messages).rev().map(|m| ThreadMsg {
            entry_id: m.entry_id.clone(), from_name: m.sender.clone(), from: m.email.clone(), to: m.to.clone(),
            received: iso(m.received), subject: m.subject.clone(),
            body: truncate_chars(&strip_quoted(&clean_text(&m.body)), chars),
        }).collect())
    }

    fn folder_tree(&self, _max_depth: usize) -> MailResult<Vec<FolderNode>> {
        let flat = self.list_folders(9)?;
        fn build(flat: &[Folder], prefix: &str) -> Vec<FolderNode> {
            flat.iter()
                .filter(|f| match f.path.rsplit_once('/') { Some((p, _)) => p == prefix, None => prefix.is_empty() })
                .map(|f| FolderNode {
                    path: f.path.clone(), name: f.path.rsplit('/').next().unwrap().to_string(),
                    special: match f.path.as_str() {
                        "Inbox" => Some("inbox".into()), "Drafts" => Some("drafts".into()),
                        "Sent Items" => Some("sent".into()), "Deleted Items" => Some("deleted".into()), _ => None,
                    },
                    unread: f.unread, total: None, children: build(flat, &f.path),
                }).collect()
        }
        Ok(build(&flat, ""))
    }

    fn page(&self, folder: &str, offset: usize, limit: usize) -> MailResult<(Vec<Summary>, bool)> {
        let all = self.list_messages(folder, None, false, usize::MAX)?;
        let more = all.len() > offset + limit;
        Ok((all.into_iter().skip(offset).take(limit).collect(), more))
    }

    fn read_full(&self, entry_id: &str) -> MailResult<FullMessage> {
        let m = self.get(entry_id)?;
        Ok(FullMessage {
            entry_id: m.entry_id.clone(), kind: "email".into(), subject: m.subject.clone(), from_name: m.sender.clone(),
            from: m.email.clone(), to: m.to.clone(), cc: m.cc.clone(), received: iso(m.received), importance: "normal".into(),
            unread: m.unread, is_draft: m.folder == "Drafts", html: m.html.clone(), text: clean_text(&m.body),
            attachments: m.attachments.iter().enumerate()
                .map(|(i, n)| AttachmentInfo { index: i as i64 + 1, name: n.clone(), size: 1234 }).collect(),
            last_action: m.last_verb.as_ref().and_then(|(c, _)| verb_name(*c)).map(String::from),
            last_action_at: m.last_verb.as_ref().map(|(_, t)| t.clone()),
        })
    }

    fn save_attachment(&self, entry_id: &str, index: i64, dir: &std::path::Path) -> MailResult<std::path::PathBuf> {
        let m = self.get(entry_id)?;
        let name = m.attachments.get((index - 1).max(0) as usize)
            .ok_or_else(|| MailError::NotFound(format!("Attachment {index} not found.")))?;
        std::fs::create_dir_all(dir).map_err(|e| MailError::Outlook(e.to_string()))?;
        let path = dir.join(safe_file_name(name));
        let content: &[u8] = if name.ends_with(".pdf") { DEMO_PDF } else if name.ends_with(".txt") {
            "Pricing notes (sample)\n\n1. Launch prices are valid until 31 December.\n2. Volume discounts are reviewed quarterly.\nملاحظة: هذا ملف تجريبي.\n".as_bytes()
        } else { b"sample attachment" };
        std::fs::write(&path, content).map_err(|e| MailError::Outlook(e.to_string()))?;
        Ok(path)
    }

    fn resolve_name(&self, name: &str) -> MailResult<Option<(String, String)>> {
        Ok(resolve(name).filter(|(_, e)| e.contains('@') && !name.contains('@')))
    }

    fn mark_read(&self, entry_id: &str) -> MailResult<bool> {
        let mut st = self.state.lock().unwrap();
        let m = st.iter_mut().find(|m| m.entry_id == entry_id)
            .ok_or_else(|| MailError::NotFound("Email not found (it may have been moved or deleted).".into()))?;
        Ok(std::mem::replace(&mut m.unread, false))
    }

    fn display(&self, entry_id: &str) -> MailResult<()> {
        self.get(entry_id).map(|_| ())
    }

    fn list_drafts(&self, limit: usize) -> MailResult<Vec<DraftInfo>> {
        let mut d = self.drafts();
        d.sort_by(|a, b| b.modified.cmp(&a.modified));
        Ok(d.iter().take(limit).map(|m| DraftInfo {
            entry_id: m.entry_id.clone(), subject: m.subject.clone(), to: m.to.clone(),
            modified: iso(Some(m.modified)), preview: String::new(),
        }).collect())
    }

    fn create_reply_draft(&self, entry_id: &str, text: &DraftText, reply_all: bool) -> MailResult<SavedDraft> {
        if text.body.trim().is_empty() {
            return Err(MailError::Invalid("Reply body is empty.".into()));
        }
        let m = self.get(entry_id)?;
        let quoted = format!("<html><head></head><body lang=EN><hr><p>From: {}</p>{}</body></html>", m.sender, m.html);
        let to = if reply_all { format!("{}; Team", m.sender) } else { m.sender.clone() };
        let html = insert_styled(Some(&quoted), &text.render(&split_recipients(&to)), &text.block_style());
        Ok(self.save_draft(format!("RE: {}", m.subject), to, String::new(), html))
    }

    fn whoami(&self) -> MailResult<(String, String)> {
        Ok(("Adel Ahmed".into(), "adel@example.com".into()))
    }

    fn sent_items(&self, offset: usize, limit: usize) -> MailResult<(Vec<SentItem>, bool)> {
        let mut sent: Vec<FakeItem> = self.state.lock().unwrap().iter().filter(|m| m.folder == "Sent Items").cloned().collect();
        sent.sort_by(|a, b| b.received.cmp(&a.received));
        let more = sent.len() > offset + limit;
        Ok((sent.iter().skip(offset).take(limit).map(|m| SentItem {
            entry_id: m.entry_id.clone(), sent_at: iso(m.received), to: m.to.clone(), subject: m.subject.clone(), body: m.body.clone(),
            recipients: split_recipients(&m.to).iter().map(|n| (n.clone(), email_of(n))).collect(),
        }).collect(), more))
    }

    fn recent_senders(&self, offset: usize, limit: usize) -> MailResult<(Vec<Sender>, bool)> {
        let mut inbox: Vec<FakeItem> = self.state.lock().unwrap().iter().filter(|m| m.folder.starts_with("Inbox")).cloned().collect();
        inbox.sort_by(|a, b| b.received.cmp(&a.received));
        let more = inbox.len() > offset + limit;
        Ok((inbox.iter().skip(offset).take(limit).map(|m| Sender {
            item_id: m.entry_id.clone(), name: m.sender.clone(), email: m.email.clone(), when: iso(m.received),
        }).collect(), more))
    }

    fn create_forward_draft(&self, entry_id: &str, to: &str, text: &DraftText, cc: &str) -> MailResult<ForwardDraft> {
        let (to_names, cc_names) = (split_recipients(to), split_recipients(cc));
        if to_names.is_empty() {
            return Err(MailError::Invalid("A forward needs at least one recipient in 'to'.".into()));
        }
        let m = self.get(entry_id)?;
        let (mut recipients, mut unresolved) = (Vec::new(), Vec::new());
        let mut field = |names: &[String], prefix: &str| -> String {
            names.iter().map(|n| match resolve(n) {
                Some((name, smtp)) => { recipients.push(format!("{prefix}{name} <{smtp}>")); name.to_string() }
                None => { unresolved.push(n.clone()); n.clone() }
            }).collect::<Vec<_>>().join("; ")
        };
        let (to_field, cc_field) = (field(&to_names, ""), field(&cc_names, "CC: "));
        let quoted = format!("<html><head></head><body lang=EN><hr><p>From: {}</p>{}</body></html>", m.sender, m.html);
        let to_display: Vec<String> = to_field.split("; ").map(String::from).collect();
        let meeting = m.class == 53;
        // A meeting invitation becomes a normal email with the invitation attached (like the Outlook layer).
        let base = if meeting {
            let bs = text.block_style();
            let arabic = bs.rtl || crate::text::has_arabic(&text.body);
            format!("<html><body>{}</body></html>", crate::text::invitation_details(&m.subject, None, None, arabic, &bs.font_family))
        } else { quoted };
        let html = if text.body.trim().is_empty() { base } else {
            insert_styled(Some(&base), &text.render(&to_display), &text.block_style())
        };
        let subject = format!("FW: {}", m.subject);
        let attachments = if meeting { vec![format!("{}.msg", m.subject)] } else { m.attachments.clone() };
        let saved = self.save_draft(subject.clone(), to_field, cc_field, html);
        {
            let mut st = self.state.lock().unwrap();
            if let Some(d) = st.iter_mut().find(|d| d.entry_id == saved.entry_id) {
                d.attachments = attachments.clone(); // a forward keeps the attachments / carries the invitation
            }
        }
        Ok(ForwardDraft { entry_id: saved.entry_id, subject, recipients, attachments: attachments.len() as i64, unresolved })
    }

    fn create_new_draft(&self, to: &str, subject: &str, text: &DraftText, cc: &str) -> MailResult<ForwardDraft> {
        let (to_names, cc_names) = (split_recipients(to), split_recipients(cc));
        if to_names.is_empty() {
            return Err(MailError::Invalid("A draft needs at least one recipient in 'to'.".into()));
        }
        let (mut recipients, mut unresolved) = (Vec::new(), Vec::new());
        let mut field = |names: &[String], prefix: &str| -> Vec<String> {
            names.iter().map(|n| match resolve(n) {
                Some((name, smtp)) => { recipients.push(format!("{prefix}{name} <{smtp}>")); name }
                None => { unresolved.push(n.clone()); n.clone() }
            }).collect()
        };
        let (to_display, cc_display) = (field(&to_names, ""), field(&cc_names, "CC: "));
        let html = insert_styled(Some("<html><body></body></html>"), &text.render(&to_display), &text.block_style());
        let saved = self.save_draft(subject.into(), to_display.join("; "), cc_display.join("; "), html);
        Ok(ForwardDraft { entry_id: saved.entry_id, subject: subject.into(), recipients, attachments: 0, unresolved })
    }
}
