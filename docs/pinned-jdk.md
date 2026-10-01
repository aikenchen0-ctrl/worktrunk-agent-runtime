# 固定 Windows JDK 17 的构建入口

需要遵守固定 JDK 17 合同的会话，在本机配置中同时设置 JAVA_HOME 和 AGENT_JAVA_HOME 为同一个绝对路径。agentctl 校验该目录的 release 声明、bin/java.exe 和项目 Wrapper JAR，直接执行 Java WrapperMain；校验失败时拒绝启动，不回退到 PATH。

未设置 AGENT_JAVA_HOME 的既有消费者继续使用原 Wrapper 启动方式。固定入口仍在原有 Windows Job Object、构建锁、进程限额、私有 Gradle/Maven 目录和设备租约之内运行。错误路径同样释放本次持有的操作锁。

安装采用版本化二进制路径和 SHA-256 校验，不覆盖其他会话正在运行的程序。源码候选和已安装版本必须分别记录，不能仅凭 0.1.0 版本字符串判断功能相同。

回归包括子命令帮助无运行时副作用、固定 JDK 拒绝错误版本、参数边界保真、锁及消费者事件测试，以及真实 Windows JDK 17 项目构建。帮助命令成功不构成项目测试通过。
