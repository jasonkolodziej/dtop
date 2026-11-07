import Foundation
import DtopApp

let status = await DtopMain.run(arguments: Array(CommandLine.arguments.dropFirst()))
exit(status)
