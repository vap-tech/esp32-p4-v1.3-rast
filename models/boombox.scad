// Integrated display + two separate speaker enclosures. All dimensions mm.
// Design coordinates: X horizontal, Y along front face upwards, Z into case.
// Assembly and body rest on XY at world Z=0. Front and rear walls slope equally towards each other.
part = "assembly"; // [assembly,body]
show_display = true;
show_speakers = true;
$fn=64;
angle=-15;
wall=3;
face_h=122;
center_w=190;
pod_w=72;
pod_rear_taper=0;
depth=58;
rear_top_drop=0;

// Dimensions carried over from original stand; standoff size still provisional.
board_w=182.5;
board_h=114;
board_t=8;
board_bottom=4;
board_clearance=0.6;
mount_dx=167;
mount_dy=100;
mount_bottom=7;
peg_d=6;
peg_length=10;
peg_fit=0.4;
mount_t=4;
speaker_od=64.15;
speaker_cutout=51.71;
speaker_depth=29;
speaker_rim=3.2;
speaker_bcd=56.49;
speaker_screw_d=2.5;
speaker_pilot=10;
function outer_x(n)=center_w/2+pod_w-pod_rear_taper*n/depth;
function bottom_y(n)=-n*tan(angle);
function top_y(n)=face_h-n*tan(angle)-rear_top_drop*n/depth;
function speaker_x()=center_w/2+pod_w/2;
// World bottom plane lies at Z=0.
module upright() { rotate([90-angle,0,0]) children(); }
module side(s=1) { if(s<0) mirror([1,0,0]) children(); else children(); }
// Preserve previous upper depth, widen the rear of the footprint.
case_height=face_h*cos(angle);
rear_slope=tan(abs(angle));
foot_depth=depth/cos(angle)+case_height*(rear_slope-tan(angle));
module pod_volume(inset=0, left=center_w/2, right=center_w/2+pod_w) {
    x0=left+inset;
    x1=right-inset;
    z0=inset;
    z1=case_height-inset;
    f0=z0*tan(angle)-inset/cos(angle);
    f1=z1*tan(angle)-inset/cos(angle);
    b0=-foot_depth+z0*rear_slope+inset/cos(angle);
    b1=-foot_depth+z1*rear_slope+inset/cos(angle);
    // Construct in world coordinates; return to front-face design frame.
    rotate([-(90-angle),0,0])
        polyhedron(points=[
            [x0,f0,z0],[x1,f0,z0],[x1,b0,z0],[x0,b0,z0],
            [x0,f1,z1],[x1,f1,z1],[x1,b1,z1],[x0,b1,z1]],
            faces=[[3,2,1,0],[5,6,7,4],[1,5,4,0],
                   [2,6,5,1],[3,7,6,2],[0,4,7,3]],convexity=10);
}
module pod() {
    difference() {
        union() {
            difference() { pod_volume(); pod_volume(wall); }
            for(a=[45:90:315])
                translate([speaker_x()+sin(a)*speaker_bcd/2,
                           face_h/2+cos(a)*speaker_bcd/2,0])
                    cylinder(d=9,h=13);
        }
        translate([speaker_x(),face_h/2,-1]) cylinder(d=speaker_cutout,h=speaker_depth+2);
        for(a=[45:90:315])
            translate([speaker_x()+sin(a)*speaker_bcd/2,
                       face_h/2+cos(a)*speaker_bcd/2,-0.01])
                cylinder(d=speaker_screw_d,h=speaker_pilot+0.01);
        // Wire into the open display cavity. Seal after wiring.
        translate([center_w/2-1,35-2.5,35-2.5]) cube([wall+2,5,5]);
    }
}
module display_mounts() {
    for(x=[-mount_dx/2,mount_dx/2]) for(y=[board_bottom+mount_bottom,
                                        board_bottom+mount_bottom+mount_dy]) {
        difference() {
            hull() {
                translate([x,y,board_t]) cylinder(d=12,h=peg_length+mount_t);
                translate([x<0 ? -center_w/2 : center_w/2-3,y-6,board_t])
                    cube([3,12,peg_length+mount_t]);
            }
            // Display standoffs slide into these sockets, seat at their ends.
            translate([x,y,board_t-0.01]) cylinder(d=peg_d+peg_fit,h=peg_length+0.01);
            translate([x,y,board_t-1]) cylinder(d=2.4,h=peg_length+mount_t+2);
            // M2 cylindrical head, rear access.
            translate([x,y,board_t+peg_length+mount_t-2.2]) cylinder(d=4.2,h=3);
        }
    }
}
module center_frame() {
    difference() {
        translate([-center_w/2-0.5,0,0]) cube([center_w+1,face_h,wall]);
        // Full display outline cutout; four rear sockets carry the display.
        translate([-(board_w+board_clearance)/2,board_bottom-board_clearance/2,-1])
            cube([board_w+board_clearance,board_h+board_clearance,wall+2]);
        // No top crossbar: buttons and SD remain accessible.
        translate([-(board_w+board_clearance)/2,board_bottom+board_h,-1])
            cube([board_w+board_clearance,face_h,wall+2]);
    }
    display_mounts();
    // Continuous floor across full footprint, flush with pod bottoms.
    intersection() {
        pod_volume(0,-center_w/2-0.5,center_w/2+0.5);
        rotate([-(90-angle),0,0])
            translate([-center_w/2-1,-foot_depth-1,0])
                cube([center_w+2,foot_depth+2,wall]);
    }
    // Side rails integrate the four sockets with the facade and pod walls.
    for(s=[-1,1]) side(s)
        translate([center_w/2-3.4,0,0]) cube([3.9,face_h,board_t+peg_length+mount_t]);
}
module body() {
    // Trim frame, rails and floor to one common outer contour.
    intersection() {
        union() { center_frame(); side(1) pod(); side(-1) pod(); }
        pod_volume(0,-center_w/2-pod_w,center_w/2+pod_w);
    }
}
module equipment() {
    if(show_display) color([0.08,0.2,0.25,0.65]) {
        translate([-board_w/2,board_bottom,0]) cube([board_w,board_h,board_t]);
        translate([-124.5/2,board_bottom+(board_h-74.6)/2,board_t]) cube([124.5,74.6,9]);
        for(x=[-mount_dx/2,mount_dx/2]) for(y=[board_bottom+7,board_bottom+107])
            translate([x,y,board_t]) cylinder(d=peg_d,h=peg_length);
    }
    if(show_speakers) for(s=[-1,1]) side(s) color([0.2,0.2,0.2,0.65]) {
        translate([speaker_x(),face_h/2,-speaker_rim]) cylinder(d=speaker_od,h=speaker_rim);
        translate([speaker_x(),face_h/2,0]) cylinder(d=speaker_cutout,h=speaker_depth-speaker_rim);
    }
}
if(part=="assembly") {
    upright() {
        color("LightSlateGray") body();
        equipment();
    }
} else if(part=="body") upright() body();
