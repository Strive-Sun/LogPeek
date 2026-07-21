## ADDED Requirements

### Requirement: 目录变化必须关联已打开工作区

目录 watcher MUST 将修改、删除和重命名事件关联到已打开选项卡的最外层磁盘源，并对同一事件风暴执行去重。

#### Scenario: 嵌套归档源变化

- **WHEN** `outer.zip::inner.tar::app.log` 已打开且 `outer.zip` 被修改或删除
- **THEN** 系统将变化应用于所有以 `outer.zip` 为磁盘源的选项卡
- **AND** 不为每个嵌套条目重复显示相同提示

#### Scenario: 应用自身删除

- **WHEN** 用户通过应用内删除确认移除一个已打开文件
- **THEN** 应用关闭对应选项卡并完成原有删除流程
- **AND** watcher 随后产生的删除事件不再显示额外的外部删除提示
