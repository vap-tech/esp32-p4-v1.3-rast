// Stepped TPU spacer. Print wide flange down. Dimensions mm.
// Narrow section fits INSIDE rolled speaker rim; flange stays outside it.
$fn=96;
module speaker_accent_ring(seat_d=62.9, bore_d=51.71, seat_h=2.8,
                          flange_d=68, flange_h=1.2,
                          screw_bcd=56.49, screw_d=3) {
    difference() {
        union() {
            cylinder(d=flange_d,h=flange_h);
            translate([0,0,flange_h]) cylinder(d=seat_d,h=seat_h);
        }
        translate([0,0,-1]) cylinder(d=bore_d,h=flange_h+seat_h+2);
        for(a=[45:90:315])
            translate([sin(a)*screw_bcd/2,cos(a)*screw_bcd/2,-1])
                cylinder(d=screw_d,h=flange_h+seat_h+2);
    }
}
speaker_accent_ring();
