export class Widget {
  save() {}
}
export class Gadget {
  save() {}
}
export function dynCaller(obj: any) {
  obj.save();
}
